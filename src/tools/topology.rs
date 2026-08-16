use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    tools::{Tool, ToolInvocation, ToolOutput, ToolRisk},
    topology::{TopologyQuery, project_topology, query_topology},
};

pub struct TopologyQueryTool;

#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    max_nodes: Option<usize>,
    #[serde(default)]
    include_stale: Option<bool>,
}

#[async_trait]
impl Tool for TopologyQueryTool {
    fn name(&self) -> &str {
        "topology_query"
    }

    fn description(&self) -> &str {
        "Return the current thread's service topology projection with evidence IDs and TTL. This is not a CMDB."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "depth": {"type": "integer", "minimum": 1, "maximum": 4},
                "max_nodes": {"type": "integer", "minimum": 1, "maximum": 64},
                "include_stale": {"type": "boolean"}
            },
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        _arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        Err(OpsCodexError::Tool(
            "topology_query requires an investigation context".into(),
        ))
    }

    async fn execute_with_context(
        &self,
        arguments: Value,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput> {
        if invocation.cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        let arguments: Arguments = serde_json::from_value(arguments).unwrap_or(Arguments {
            depth: None,
            max_nodes: None,
            include_stale: None,
        });
        let store = invocation
            .store
            .ok_or_else(|| OpsCodexError::Tool("topology_query requires an event store".into()))?;
        let events = store.events_after(&invocation.thread_id, 0).await?;
        let graph = query_topology(
            project_topology(&invocation.workspace_id, &events),
            TopologyQuery {
                depth: arguments.depth.unwrap_or(2),
                max_nodes: arguments.max_nodes.unwrap_or(32),
                include_stale: arguments.include_stale.unwrap_or(false),
            },
        );
        Ok(ToolOutput {
            content: serde_json::to_value(&graph).unwrap_or_else(|_| json!({})),
            evidence: EvidenceMeta::new("topology")
                .with_query(format!("workspace {}", invocation.workspace_id))
                .with_summary(format!(
                    "{} nodes, {} edges",
                    graph.nodes.len(),
                    graph.edges.len()
                )),
        })
    }
}
