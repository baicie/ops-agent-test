use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    runtime::EvidenceMeta,
    tools::{Tool, ToolOutput, ToolRisk},
};

pub struct FakeTool {
    name: String,
    output: Value,
    risk: ToolRisk,
}

impl FakeTool {
    pub fn new(name: impl Into<String>, output: Value, risk: ToolRisk) -> Self {
        Self {
            name: name.into(),
            output,
            risk,
        }
    }

    pub fn safe(name: impl Into<String>, output: Value) -> Self {
        Self::new(name, output, ToolRisk::Safe)
    }

    pub fn ask(name: impl Into<String>, output: Value) -> Self {
        Self::new(name, output, ToolRisk::Ask)
    }
}

#[async_trait]
impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Return deterministic evidence for tests"
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": true})
    }

    fn risk(&self) -> ToolRisk {
        self.risk
    }

    async fn execute(
        &self,
        _arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        Ok(ToolOutput {
            content: self.output.clone(),
            evidence: EvidenceMeta {
                source: self.name.clone(),
                query: None,
                timestamp: Utc::now(),
                duration_ms: 0,
                truncated: false,
            },
        })
    }
}
