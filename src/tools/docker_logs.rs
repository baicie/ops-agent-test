use std::{collections::HashSet, path::PathBuf, time::Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    tools::{Tool, ToolOutput, ToolRisk, truncate_output},
};

pub struct DockerLogsTool {
    allowed_containers: HashSet<String>,
    docker_binary: PathBuf,
    max_output_bytes: usize,
}

impl DockerLogsTool {
    pub fn new<I, S>(allowed_containers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed_containers: allowed_containers.into_iter().map(Into::into).collect(),
            docker_binary: PathBuf::from("docker"),
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_binary(mut self, docker_binary: impl Into<PathBuf>) -> Self {
        self.docker_binary = docker_binary.into();
        self
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

fn default_since() -> String {
    "10m".into()
}

const fn default_tail() -> u32 {
    200
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerLogsArguments {
    container: String,
    #[serde(default = "default_since")]
    since: String,
    #[serde(default = "default_tail")]
    tail: u32,
}

#[async_trait]
impl Tool for DockerLogsTool {
    fn name(&self) -> &str {
        "docker_logs"
    }

    fn description(&self) -> &str {
        "Read recent logs from an allowlisted Docker container"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "container": {"type": "string"},
                "since": {"type": "string", "default": "10m"},
                "tail": {"type": "integer", "minimum": 0, "maximum": 10000, "default": 200}
            },
            "required": ["container"],
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        let arguments: DockerLogsArguments =
            serde_json::from_value(arguments).map_err(|error| {
                OpsCodexError::Tool(format!("invalid docker_logs arguments: {error}"))
            })?;
        if !self.allowed_containers.contains(&arguments.container) {
            return Err(OpsCodexError::Policy(format!(
                "container `{}` is not allowlisted",
                arguments.container
            )));
        }
        if arguments.since.is_empty() || arguments.since.len() > 128 {
            return Err(OpsCodexError::Tool(
                "docker_logs since must contain 1 to 128 characters".into(),
            ));
        }
        if arguments.tail > 10_000 {
            return Err(OpsCodexError::Tool(
                "docker_logs tail cannot exceed 10000".into(),
            ));
        }

        let started = Instant::now();
        let mut command = Command::new(&self.docker_binary);
        command
            .kill_on_drop(true)
            .args(["logs", "--since", &arguments.since, "--tail"])
            .arg(arguments.tail.to_string())
            .arg(&arguments.container);
        let output = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            output = command.output() => output
                .map_err(|error| OpsCodexError::Tool(format!("failed to run docker logs: {error}")))?,
        };
        let mut bytes = output.stdout;
        if !output.stderr.is_empty() {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&output.stderr);
        }
        let (logs, truncated) = truncate_output(&bytes, self.max_output_bytes);
        if !output.status.success() {
            return Err(OpsCodexError::Tool(format!(
                "docker logs exited with {}: {logs}",
                output.status
            )));
        }

        Ok(ToolOutput {
            content: json!({"container": arguments.container, "logs": logs}),
            evidence: EvidenceMeta::new("docker")
                .with_query(format!(
                    "docker logs --since {} --tail {} {}",
                    arguments.since, arguments.tail, arguments.container
                ))
                .with_duration_ms(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                .with_truncated(truncated),
        })
    }
}
