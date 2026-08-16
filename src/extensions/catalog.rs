use std::{path::PathBuf, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    config::{Config, ExtensionConfigEntry},
    extensions::{
        CapabilityEffect, ChildSupervisor, CustomJsonTool, ExtensionHealth, ExtensionState,
        ExtensionSummary, McpHttpTool, McpInstallSpec, McpStdioClient, McpTool, RecoveryMode,
        SkillCatalog, load_custom_manifest, mcp_http_initialize, mcp_http_list_tools,
        validate_mcp_http_url, validate_path,
    },
    runtime::WorkspaceId,
    tools::{Tool, ToolRegistry},
    workspace::Workspace,
};

#[derive(Clone, Debug, Default)]
pub struct ExtensionCatalog {
    summaries: Vec<ExtensionSummary>,
}

impl ExtensionCatalog {
    pub fn summaries(&self) -> &[ExtensionSummary] {
        &self.summaries
    }

    pub fn for_workspace(&self, workspace_id: &str) -> Vec<&ExtensionSummary> {
        self.summaries
            .iter()
            .filter(|item| {
                item.workspaces.is_empty()
                    || item
                        .workspaces
                        .iter()
                        .any(|workspace| workspace == workspace_id)
            })
            .collect()
    }

    pub async fn install_into(
        &mut self,
        tools: &mut ToolRegistry,
        config: &Config,
        workspace: &Workspace,
        client: &reqwest::Client,
    ) -> SkillCatalog {
        let workspace_id = workspace.id.as_str();
        for entry in &config.extension {
            if !entry.enabled || !applies_to(entry, workspace_id) {
                continue;
            }
            match install_extension(tools, config, workspace, client, entry).await {
                Ok(summary) => self.summaries.push(summary),
                Err(error) => {
                    tracing::warn!(
                        extension = %entry.id,
                        workspace = workspace_id,
                        error = %error,
                        "extension failed closed and was not enabled"
                    );
                    self.summaries
                        .push(disabled_summary(entry, workspace_id, error));
                }
            }
        }
        load_skills(config, &workspace.id)
    }
}

fn applies_to(entry: &ExtensionConfigEntry, workspace_id: &str) -> bool {
    entry.workspaces.is_empty() || entry.workspaces.iter().any(|item| item == workspace_id)
}

async fn install_extension(
    tools: &mut ToolRegistry,
    config: &Config,
    workspace: &Workspace,
    client: &reqwest::Client,
    entry: &ExtensionConfigEntry,
) -> Result<ExtensionSummary> {
    match entry.kind.as_str() {
        "custom" => install_custom(tools, config, workspace, entry),
        "mcp" => install_mcp_stdio(tools, config, workspace, entry).await,
        "mcp_http" => install_mcp_http(tools, config, workspace, client, entry).await,
        other => Err(OpsCodexError::Protocol(format!(
            "unsupported extension kind `{other}`"
        ))),
    }
}

fn install_custom(
    tools: &mut ToolRegistry,
    config: &Config,
    workspace: &Workspace,
    entry: &ExtensionConfigEntry,
) -> Result<ExtensionSummary> {
    if config.extensions.production_safe || !config.extensions.allow_custom_tools {
        return Err(OpsCodexError::Policy(
            "custom tools are disabled by extensions policy".into(),
        ));
    }
    if !entry.trusted_local {
        return Err(OpsCodexError::Policy(
            "custom tools require trusted_local".into(),
        ));
    }
    let path = entry.path.as_deref().ok_or_else(|| {
        OpsCodexError::Protocol(format!("extension `{}` is missing path", entry.id))
    })?;
    let manifest = load_custom_manifest(path)?;
    let env = collect_env(&entry.env);
    let effect = parse_effect(entry.effect.as_deref())?;
    let tool = CustomJsonTool::from_manifest(
        manifest,
        entry.trusted_local,
        effect,
        workspace.max_effect,
        env,
        entry.max_restarts.unwrap_or(2),
    )?;
    let descriptor = tool.descriptor();
    tools.register(Arc::new(tool))?;
    Ok(extension_summary(
        entry,
        "custom",
        &descriptor.provenance.version,
        &descriptor.provenance.schema_hash,
        workspace.id.as_str(),
        ExtensionHealth::healthy(),
        vec![descriptor.summary()],
    ))
}

async fn install_mcp_stdio(
    tools: &mut ToolRegistry,
    config: &Config,
    workspace: &Workspace,
    entry: &ExtensionConfigEntry,
) -> Result<ExtensionSummary> {
    if config.extensions.production_safe {
        return Err(OpsCodexError::Policy(
            "stdio MCP is disabled by production_safe".into(),
        ));
    }
    let command = PathBuf::from(entry.command.as_deref().ok_or_else(|| {
        OpsCodexError::Protocol(format!("extension `{}` is missing command", entry.id))
    })?);
    if let Some(cwd) = &entry.cwd {
        validate_path(PathBuf::from(cwd).as_path())?;
    }
    let supervisor = ChildSupervisor::new(entry.max_restarts.unwrap_or(2));
    let mut last_error = OpsCodexError::Tool("MCP stdio server failed".into());
    for _ in 0..=supervisor.max_restarts {
        match McpStdioClient::spawn(
            command.clone(),
            entry.args.clone(),
            entry.cwd.as_ref().map(PathBuf::from),
        )
        .await
        {
            Ok(client) => {
                return register_stdio_tools(tools, workspace, entry, client).await;
            }
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

async fn register_stdio_tools(
    tools: &mut ToolRegistry,
    workspace: &Workspace,
    entry: &ExtensionConfigEntry,
    mut client: McpStdioClient,
) -> Result<ExtensionSummary> {
    let listed = client.list_tools().await?;
    let client = Arc::new(tokio::sync::Mutex::new(client));
    let effect = required_effect(entry)?;
    let recovery = required_recovery(entry)?;
    let version = entry.version.clone().unwrap_or_else(|| "1.0.0".into());
    let mut capabilities = Vec::new();
    for tool in listed {
        let wrapped = McpTool::new(
            McpInstallSpec {
                server_id: &entry.id,
                trusted_local: entry.trusted_local,
                effect,
                recovery,
                version: &version,
                workspace_max: workspace.max_effect,
                origin: format!("mcp-stdio:{}", entry.id),
            },
            tool,
            client.clone(),
        )?;
        capabilities.push(wrapped.descriptor().summary());
        tools.register(Arc::new(wrapped))?;
    }
    Ok(extension_summary(
        entry,
        "mcp",
        &version,
        &hash_origin(&format!("mcp-stdio:{}", entry.id)),
        workspace.id.as_str(),
        ExtensionHealth::healthy(),
        capabilities,
    ))
}

async fn install_mcp_http(
    tools: &mut ToolRegistry,
    config: &Config,
    workspace: &Workspace,
    client: &reqwest::Client,
    entry: &ExtensionConfigEntry,
) -> Result<ExtensionSummary> {
    let raw = entry.url.as_deref().ok_or_else(|| {
        OpsCodexError::Protocol(format!("extension `{}` is missing url", entry.id))
    })?;
    let allowlist = &entry.allowlist_hosts;
    if config.extensions.production_safe && allowlist.is_empty() {
        return Err(OpsCodexError::Policy(
            "production_safe MCP HTTP requires an allowlist".into(),
        ));
    }
    let endpoint = validate_mcp_http_url(raw, allowlist)?;
    let cancellation = CancellationToken::new();
    mcp_http_initialize(client, &endpoint, cancellation.clone()).await?;
    let listed = mcp_http_list_tools(client, &endpoint, cancellation).await?;
    let effect = required_effect(entry)?;
    let recovery = required_recovery(entry)?;
    let version = entry.version.clone().unwrap_or_else(|| "1.0.0".into());
    let mut capabilities = Vec::new();
    for tool in listed {
        let wrapped = McpHttpTool::new(
            McpInstallSpec {
                server_id: &entry.id,
                trusted_local: entry.trusted_local,
                effect,
                recovery,
                version: &version,
                workspace_max: workspace.max_effect,
                origin: endpoint.to_string(),
            },
            tool,
            endpoint.clone(),
            client.clone(),
        )?;
        capabilities.push(wrapped.descriptor().summary());
        tools.register(Arc::new(wrapped))?;
    }
    Ok(extension_summary(
        entry,
        "mcp_http",
        &version,
        &hash_origin(endpoint.as_str()),
        workspace.id.as_str(),
        ExtensionHealth::healthy(),
        capabilities,
    ))
}

fn load_skills(config: &Config, workspace: &WorkspaceId) -> SkillCatalog {
    let mut catalog = SkillCatalog::default();
    for entry in &config.skills {
        if !entry.enabled {
            continue;
        }
        if !entry.workspaces.is_empty()
            && !entry
                .workspaces
                .iter()
                .any(|item| item == workspace.as_str())
        {
            continue;
        }
        match crate::extensions::load_skill(&entry.path) {
            Ok(skill) => {
                if let Err(error) = catalog.insert(skill) {
                    tracing::warn!(path = %entry.path, error = %error, "duplicate skill skipped");
                }
            }
            Err(error) => {
                tracing::warn!(path = %entry.path, error = %error, "skill failed closed");
            }
        }
    }
    catalog
}

fn required_effect(entry: &ExtensionConfigEntry) -> Result<CapabilityEffect> {
    entry
        .effect
        .as_deref()
        .map(CapabilityEffect::parse)
        .transpose()?
        .ok_or_else(|| {
            OpsCodexError::Policy(format!(
                "extension `{}` requires a local effect override",
                entry.id
            ))
        })
}

fn required_recovery(entry: &ExtensionConfigEntry) -> Result<RecoveryMode> {
    entry
        .recovery
        .as_deref()
        .map(RecoveryMode::parse)
        .transpose()?
        .ok_or_else(|| {
            OpsCodexError::Policy(format!(
                "extension `{}` requires local recovery metadata",
                entry.id
            ))
        })
}

fn parse_effect(value: Option<&str>) -> Result<Option<CapabilityEffect>> {
    value.map(CapabilityEffect::parse).transpose()
}

fn collect_env(allowlist: &[String]) -> Vec<(String, String)> {
    allowlist
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.clone(), value)))
        .collect()
}

fn hash_origin(origin: &str) -> String {
    crate::extensions::hash_bytes(origin.as_bytes())
}

fn extension_summary(
    entry: &ExtensionConfigEntry,
    kind: &str,
    version: &str,
    hash: &str,
    workspace_id: &str,
    health: ExtensionHealth,
    capabilities: Vec<crate::extensions::CapabilitySummary>,
) -> ExtensionSummary {
    ExtensionSummary {
        id: entry.id.clone(),
        kind: kind.into(),
        version: version.into(),
        hash: hash.into(),
        enabled: health.state != ExtensionState::Disabled,
        health,
        workspaces: if entry.workspaces.is_empty() {
            vec![workspace_id.to_owned()]
        } else {
            entry.workspaces.clone()
        },
        capabilities,
    }
}

fn disabled_summary(
    entry: &ExtensionConfigEntry,
    workspace_id: &str,
    error: OpsCodexError,
) -> ExtensionSummary {
    extension_summary(
        entry,
        &entry.kind,
        entry.version.as_deref().unwrap_or("unknown"),
        "unpinned",
        workspace_id,
        ExtensionHealth::disabled(error.to_string()),
        Vec::new(),
    )
}
