use std::time::Instant;

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

pub struct ExecTool {
    max_output_bytes: usize,
}

impl ExecTool {
    pub fn new() -> Self {
        Self {
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArguments {
    command: String,
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command after explicit user approval"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"}
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Ask
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        let arguments: ExecArguments = serde_json::from_value(arguments)
            .map_err(|error| OpsCodexError::Tool(format!("invalid exec arguments: {error}")))?;
        if arguments.command.trim().is_empty() {
            return Err(OpsCodexError::Tool("exec command cannot be empty".into()));
        }
        if arguments.command.len() > 16 * 1024 {
            return Err(OpsCodexError::Tool(
                "exec command cannot exceed 16 KiB".into(),
            ));
        }

        let started = Instant::now();
        let mut command = Command::new("sh");
        command.kill_on_drop(true).arg("-c").arg(&arguments.command);
        let output = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            output = command.output() => output
                .map_err(|error| OpsCodexError::Tool(format!("failed to execute command: {error}")))?,
        };
        let mut bytes = output.stdout;
        if !output.stderr.is_empty() {
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(&output.stderr);
        }
        let (text, truncated) = truncate_output(&bytes, self.max_output_bytes);

        if !output.status.success() {
            return Err(OpsCodexError::Tool(format!(
                "command exited with {}: {text}",
                output.status
            )));
        }

        Ok(ToolOutput {
            content: json!({
                "command": arguments.command,
                "exit_code": output.status.code(),
                "output": text
            }),
            evidence: EvidenceMeta::new("exec")
                .with_query(arguments.command)
                .with_duration_ms(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                .with_truncated(truncated),
        })
    }
}
