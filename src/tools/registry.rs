use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    model::ToolSchema,
    tools::{Tool, ToolOutput, ToolRisk},
};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.name();
        if name.trim().is_empty() {
            return Err(OpsCodexError::Tool("tool name cannot be empty".into()));
        }
        if name != name.trim() {
            return Err(OpsCodexError::Tool(format!(
                "tool name `{name}` cannot have surrounding whitespace"
            )));
        }
        if self.tools.contains_key(name) {
            return Err(OpsCodexError::Tool(format!(
                "tool `{name}` is already registered"
            )));
        }
        self.tools.insert(name.to_owned(), tool);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut tools: Vec<_> = self.tools.values().collect();
        tools.sort_unstable_by(|left, right| left.name().cmp(right.name()));
        tools
            .into_iter()
            .map(|tool| ToolSchema {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.schema(),
            })
            .collect()
    }

    pub fn risk(&self, name: &str) -> Result<ToolRisk> {
        self.tool(name).map(|tool| tool.risk())
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        self.tool(name)?.execute(arguments, cancellation).await
    }

    fn tool(&self, name: &str) -> Result<&Arc<dyn Tool>> {
        self.tools
            .get(name)
            .ok_or_else(|| OpsCodexError::Tool(format!("unknown tool `{name}`")))
    }
}
