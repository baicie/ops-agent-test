use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    extensions::{
        CapabilityDescriptor, CapabilityEffect, CapabilitySource, Provenance, RecoveryMode,
        capability_id, hash_schema, validate_command,
    },
    tools::{Tool, ToolOutput, ToolRisk},
};

pub struct McpStdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpStdioClient {
    pub async fn spawn(command: PathBuf, args: Vec<String>, cwd: Option<PathBuf>) -> Result<Self> {
        validate_command(&command)?;
        let mut process = Command::new(&command);
        process
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        let mut child = process
            .spawn()
            .map_err(|error| OpsCodexError::Tool(format!("failed to start MCP server: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| OpsCodexError::Tool("MCP server stdin is unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OpsCodexError::Tool("MCP server stdout is unavailable".into()))?;
        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "opscodex", "version": "0.1.0"}
                }),
            )
            .await?;
        if init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(OpsCodexError::Protocol(
                "MCP initialize did not return a protocolVersion".into(),
            ));
        }
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    pub async fn list_tools(&mut self) -> Result<Vec<McpListedTool>> {
        parse_listed_tools(self.request("tools/list", json!({})).await?)
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments
            }),
        )
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        loop {
            let response = self.read().await?;
            if response.get("id").and_then(Value::as_u64) == Some(id)
                || response.get("id").and_then(Value::as_i64) == Some(id as i64)
            {
                if let Some(error) = response.get("error") {
                    return Err(OpsCodexError::Tool(format!("MCP error: {error}")));
                }
                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn write(&mut self, message: &Value) -> Result<()> {
        let encoded = serde_json::to_vec(message).unwrap_or_default();
        let header = format!("Content-Length: {}\r\n\r\n", encoded.len());
        self.stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|error| {
                OpsCodexError::Tool(format!("failed to write MCP request: {error}"))
            })?;
        self.stdin.write_all(&encoded).await.map_err(|error| {
            OpsCodexError::Tool(format!("failed to write MCP request: {error}"))
        })?;
        self.stdin.flush().await.ok();
        Ok(())
    }

    async fn read(&mut self) -> Result<Value> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).await.map_err(|error| {
                OpsCodexError::Tool(format!("failed to read MCP response: {error}"))
            })?;
            if read == 0 {
                return Err(OpsCodexError::Tool("MCP server closed stdout".into()));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.split_once(':') {
                if value.0.eq_ignore_ascii_case("content-length") {
                    content_length = Some(value.1.trim().parse::<usize>().map_err(|_| {
                        OpsCodexError::Protocol("invalid MCP Content-Length".into())
                    })?);
                }
            } else if trimmed.starts_with('{') {
                return serde_json::from_str(trimmed).map_err(|error| {
                    OpsCodexError::Protocol(format!("invalid MCP JSON-RPC: {error}"))
                });
            }
        }
        let length = content_length.ok_or_else(|| {
            OpsCodexError::Protocol("MCP response is missing Content-Length".into())
        })?;
        let mut buf = vec![0_u8; length];
        self.stdout
            .read_exact(&mut buf)
            .await
            .map_err(|error| OpsCodexError::Tool(format!("failed to read MCP body: {error}")))?;
        serde_json::from_slice(&buf)
            .map_err(|error| OpsCodexError::Protocol(format!("invalid MCP JSON-RPC: {error}")))
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[derive(Clone, Debug)]
pub struct McpListedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpTool {
    capability: CapabilityDescriptor,
    remote_name: String,
    client: Arc<Mutex<McpStdioClient>>,
}

#[derive(Clone, Debug)]
pub struct McpInstallSpec<'a> {
    pub server_id: &'a str,
    pub trusted_local: bool,
    pub effect: CapabilityEffect,
    pub recovery: RecoveryMode,
    pub version: &'a str,
    pub workspace_max: Option<CapabilityEffect>,
    pub origin: String,
}

impl McpTool {
    pub fn new(
        spec: McpInstallSpec<'_>,
        listed: McpListedTool,
        client: Arc<Mutex<McpStdioClient>>,
    ) -> Result<Self> {
        let remote_name = listed.name.clone();
        let mut descriptor = mcp_descriptor(
            spec.server_id,
            &listed,
            spec.trusted_local,
            spec.effect,
            spec.recovery,
            spec.version,
            spec.origin,
        );
        descriptor = descriptor.apply_strictest(None, None);
        enforce_workspace_ceiling(&descriptor, spec.workspace_max)?;
        descriptor.validate_for_enablement()?;
        Ok(Self {
            capability: descriptor,
            remote_name,
            client,
        })
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.capability.name
    }

    fn description(&self) -> &str {
        &self.capability.description
    }

    fn schema(&self) -> Value {
        self.capability.input_schema.clone()
    }

    fn risk(&self) -> ToolRisk {
        match self.capability.effect {
            CapabilityEffect::Observe => ToolRisk::Safe,
            _ => ToolRisk::Ask,
        }
    }

    fn descriptor(&self) -> CapabilityDescriptor {
        self.capability.clone()
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(self.capability.timeout_seconds)) => {
                return Err(OpsCodexError::Timeout("MCP tool timed out".into()));
            }
            result = async {
                let mut client = self.client.lock().await;
                client.call_tool(&self.remote_name, arguments.clone()).await
            } => result?,
        };
        Ok(mcp_output(&self.capability, result))
    }
}

pub struct McpHttpTool {
    capability: CapabilityDescriptor,
    remote_name: String,
    endpoint: Url,
    client: reqwest::Client,
}

impl McpHttpTool {
    pub fn new(
        spec: McpInstallSpec<'_>,
        listed: McpListedTool,
        endpoint: Url,
        client: reqwest::Client,
    ) -> Result<Self> {
        let remote_name = listed.name.clone();
        let mut descriptor = mcp_descriptor(
            spec.server_id,
            &listed,
            spec.trusted_local,
            spec.effect,
            spec.recovery,
            spec.version,
            spec.origin,
        );
        descriptor = descriptor.apply_strictest(None, None);
        enforce_workspace_ceiling(&descriptor, spec.workspace_max)?;
        descriptor.validate_for_enablement()?;
        Ok(Self {
            capability: descriptor,
            remote_name,
            endpoint,
            client,
        })
    }
}

#[async_trait]
impl Tool for McpHttpTool {
    fn name(&self) -> &str {
        &self.capability.name
    }

    fn description(&self) -> &str {
        &self.capability.description
    }

    fn schema(&self) -> Value {
        self.capability.input_schema.clone()
    }

    fn risk(&self) -> ToolRisk {
        match self.capability.effect {
            CapabilityEffect::Observe => ToolRisk::Safe,
            _ => ToolRisk::Ask,
        }
    }

    fn descriptor(&self) -> CapabilityDescriptor {
        self.capability.clone()
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let result = mcp_http_call(
            &self.client,
            &self.endpoint,
            "tools/call",
            json!({
                "name": self.remote_name,
                "arguments": arguments
            }),
            cancellation,
            self.capability.timeout_seconds,
        )
        .await?;
        Ok(mcp_output(&self.capability, result))
    }
}

pub fn validate_mcp_http_url(raw: &str, allowlist: &[String]) -> Result<Url> {
    let url = Url::parse(raw)
        .map_err(|error| OpsCodexError::Protocol(format!("invalid MCP URL: {error}")))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "::1");
    if url.scheme() != "https" && !loopback {
        return Err(OpsCodexError::Policy(
            "MCP HTTP endpoints must use TLS unless they are loopback".into(),
        ));
    }
    if url.username() != "" || url.password().is_some() {
        return Err(OpsCodexError::Policy(
            "MCP URL cannot contain credentials".into(),
        ));
    }
    if allowlist.is_empty() {
        return Err(OpsCodexError::Policy(
            "MCP HTTP endpoints require an explicit host allowlist".into(),
        ));
    }
    let allowed = allowlist.iter().any(|item| item.eq_ignore_ascii_case(host));
    if !allowed {
        return Err(OpsCodexError::Policy(format!(
            "MCP host `{host}` is not in the Workspace allowlist"
        )));
    }
    Ok(url)
}

pub async fn mcp_http_initialize(
    client: &reqwest::Client,
    endpoint: &Url,
    cancellation: CancellationToken,
) -> Result<()> {
    let result = mcp_http_call(
        client,
        endpoint,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "opscodex", "version": "0.1.0"}
        }),
        cancellation.clone(),
        30,
    )
    .await?;
    if result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(OpsCodexError::Protocol(
            "MCP HTTP initialize did not return a protocolVersion".into(),
        ));
    }
    Ok(())
}

pub async fn mcp_http_list_tools(
    client: &reqwest::Client,
    endpoint: &Url,
    cancellation: CancellationToken,
) -> Result<Vec<McpListedTool>> {
    parse_listed_tools(
        mcp_http_call(client, endpoint, "tools/list", json!({}), cancellation, 30).await?,
    )
}

pub async fn mcp_http_call(
    client: &reqwest::Client,
    endpoint: &Url,
    method: &str,
    params: Value,
    cancellation: CancellationToken,
    timeout_seconds: u64,
) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });
    let request = client.post(endpoint.clone()).json(&body);
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
        _ = tokio::time::sleep(Duration::from_secs(timeout_seconds.max(1))) => {
            return Err(OpsCodexError::Timeout("MCP HTTP timed out".into()));
        }
        response = request.send() => {
            response.map_err(|error| OpsCodexError::Tool(format!("MCP HTTP failed: {error}")))?
        }
    };
    let payload: Value = response
        .json()
        .await
        .map_err(|error| OpsCodexError::Protocol(format!("invalid MCP HTTP JSON: {error}")))?;
    if let Some(error) = payload.get("error") {
        return Err(OpsCodexError::Tool(format!("MCP HTTP error: {error}")));
    }
    Ok(payload.get("result").cloned().unwrap_or(Value::Null))
}

fn parse_listed_tools(result: Value) -> Result<Vec<McpListedTool>> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tools
        .into_iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| OpsCodexError::Protocol("MCP tool is missing a name".into()))?
                .to_owned();
            Ok(McpListedTool {
                name,
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP tool")
                    .to_owned(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            })
        })
        .collect()
}

fn mcp_descriptor(
    server_id: &str,
    listed: &McpListedTool,
    trusted_local: bool,
    effect: CapabilityEffect,
    recovery: RecoveryMode,
    version: &str,
    origin: String,
) -> CapabilityDescriptor {
    let schema_hash = hash_schema(&listed.input_schema);
    CapabilityDescriptor {
        id: capability_id("mcp", &format!("{server_id}/{}", listed.name), version),
        source: CapabilitySource::Mcp,
        name: format!("mcp/{server_id}/{}", listed.name),
        description: listed.description.clone(),
        input_schema: listed.input_schema.clone(),
        output_schema: None,
        effect,
        target_requirements: Vec::new(),
        timeout_seconds: 30,
        max_output_bytes: 64 * 1024,
        recovery: Some(recovery),
        provenance: Provenance {
            source: CapabilitySource::Mcp,
            version: version.into(),
            schema_hash,
            binary_hash: None,
            origin: Some(origin),
        },
        content_sensitivity: crate::evidence::Sensitivity::Internal,
        enabled: true,
        trusted_local,
    }
}

fn mcp_output(descriptor: &CapabilityDescriptor, result: Value) -> ToolOutput {
    ToolOutput {
        content: json!({
            "source": "mcp",
            "tool": descriptor.name,
            "version": descriptor.provenance.version,
            "hash": descriptor.provenance.schema_hash,
            "result": result,
        }),
        evidence: EvidenceMeta::new("mcp")
            .with_query(descriptor.id.clone())
            .with_summary(format!("MCP {}", descriptor.name)),
    }
}

pub fn enforce_workspace_ceiling(
    descriptor: &CapabilityDescriptor,
    workspace_max: Option<CapabilityEffect>,
) -> Result<()> {
    if let Some(max) = workspace_max
        && descriptor.effect.rank() > max.rank()
    {
        return Err(OpsCodexError::Policy(format!(
            "capability `{}` effect `{}` exceeds workspace max `{}`",
            descriptor.id,
            descriptor.effect.as_str(),
            max.as_str()
        )));
    }
    Ok(())
}
