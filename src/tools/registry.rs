use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    extensions::CapabilityDescriptor,
    model::ToolSchema,
    tools::{Tool, ToolInvocation, ToolOutput, ToolRisk},
};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    ids: HashMap<String, String>,
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
        let descriptor = tool.descriptor();
        descriptor.validate_for_enablement()?;
        if self.ids.contains_key(&descriptor.id) {
            return Err(OpsCodexError::Tool(format!(
                "capability `{}` is already registered",
                descriptor.id
            )));
        }
        self.ids.insert(descriptor.id, name.to_owned());
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

    pub fn descriptor(&self, name: &str) -> Result<CapabilityDescriptor> {
        self.tool(name).map(|tool| tool.descriptor())
    }

    pub fn descriptors(&self) -> Vec<CapabilityDescriptor> {
        let mut tools: Vec<_> = self.tools.values().collect();
        tools.sort_unstable_by(|left, right| left.name().cmp(right.name()));
        tools.into_iter().map(|tool| tool.descriptor()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        self.execute_with_context(name, arguments, ToolInvocation::new(cancellation))
            .await
    }

    pub async fn execute_with_context(
        &self,
        name: &str,
        arguments: Value,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput> {
        if invocation.cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        self.tool(name)?
            .execute_with_context(arguments, invocation)
            .await
    }

    fn tool(&self, name: &str) -> Result<&Arc<dyn Tool>> {
        self.tools
            .get(name)
            .ok_or_else(|| OpsCodexError::Tool(format!("unknown tool `{name}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        extensions::CapabilityDescriptor,
        tools::{FakeTool, Tool},
    };
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct Aliased(FakeTool);

    #[async_trait::async_trait]
    impl Tool for Aliased {
        fn name(&self) -> &str {
            "inspect-alias"
        }

        fn description(&self) -> &str {
            self.0.description()
        }

        fn schema(&self) -> Value {
            self.0.schema()
        }

        fn risk(&self) -> ToolRisk {
            self.0.risk()
        }

        fn descriptor(&self) -> CapabilityDescriptor {
            let mut descriptor = self.0.descriptor();
            descriptor.id = "builtin/inspect@1.0.0".into();
            descriptor
        }

        async fn execute(
            &self,
            arguments: Value,
            cancellation: CancellationToken,
        ) -> Result<ToolOutput> {
            self.0.execute(arguments, cancellation).await
        }
    }

    #[test]
    fn register_rejects_duplicate_capability_ids() {
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(FakeTool::safe("inspect", json!({"ok": true}))))
            .unwrap();
        let error = registry
            .register(Arc::new(Aliased(FakeTool::safe(
                "inspect",
                json!({"ok": false}),
            ))))
            .unwrap_err();
        assert!(error.to_string().contains("capability"));
    }
}
