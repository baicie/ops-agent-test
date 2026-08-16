use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    runbook::RunbookCatalog,
    tools::{Tool, ToolOutput, ToolRisk},
};

pub struct RunbookSearchTool {
    catalog: Arc<RunbookCatalog>,
}

pub struct RunbookReadTool {
    catalog: Arc<RunbookCatalog>,
}

impl RunbookSearchTool {
    pub fn new(catalog: Arc<RunbookCatalog>) -> Self {
        Self { catalog }
    }
}

impl RunbookReadTool {
    pub fn new(catalog: Arc<RunbookCatalog>) -> Self {
        Self { catalog }
    }
}

#[derive(Deserialize)]
struct SearchArguments {
    #[serde(default)]
    query: String,
    #[serde(default)]
    service: Option<String>,
}

#[derive(Deserialize)]
struct ReadArguments {
    id: String,
    #[serde(default)]
    version: Option<u32>,
}

#[async_trait]
impl Tool for RunbookSearchTool {
    fn name(&self) -> &str {
        "runbook_search"
    }

    fn description(&self) -> &str {
        "Search local Markdown runbooks by service, signal, or text. Results are references, not executable commands."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "service": {"type": "string"}
            },
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
        let arguments: SearchArguments =
            serde_json::from_value(arguments).unwrap_or(SearchArguments {
                query: String::new(),
                service: None,
            });
        let matches = self
            .catalog
            .search(&arguments.query, arguments.service.as_deref());
        Ok(ToolOutput {
            content: json!({ "matches": matches }),
            evidence: EvidenceMeta::new("runbook")
                .with_query(format!("search {}", arguments.query))
                .with_summary(format!("{} runbooks matched", matches.len())),
        })
    }
}

#[async_trait]
impl Tool for RunbookReadTool {
    fn name(&self) -> &str {
        "runbook_read"
    }

    fn description(&self) -> &str {
        "Read a local runbook by stable id and version. Commands in the runbook are suggestions only."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "version": {"type": "integer", "minimum": 1}
            },
            "required": ["id"],
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
        let arguments: ReadArguments = serde_json::from_value(arguments)
            .map_err(|error| OpsCodexError::Protocol(error.to_string()))?;
        let runbook = self.catalog.read(&arguments.id, arguments.version)?;
        let body = if runbook.body.len() > 8 * 1024 {
            format!("{}…", &runbook.body[..8 * 1024])
        } else {
            runbook.body.clone()
        };
        Ok(ToolOutput {
            content: json!({
                "id": runbook.meta.id,
                "title": runbook.meta.title,
                "version": runbook.meta.version,
                "hash": runbook.meta.hash,
                "body": body,
                "note": "Runbook commands are not executed automatically."
            }),
            evidence: EvidenceMeta::new("runbook")
                .with_query(format!("{}@{}", runbook.meta.id, runbook.meta.version))
                .with_summary(format!("{} ({})", runbook.meta.title, runbook.meta.hash)),
        })
    }
}
