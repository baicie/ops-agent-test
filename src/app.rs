use std::sync::Arc;

use crate::{
    OpsCodexError, Result,
    config::Config,
    model::{DemoModelProvider, ModelProvider, OpenAIResponsesProvider},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig},
    store::JsonlStore,
    tools::{DockerLogsTool, ExecTool, HttpGetTool, PromqlTool, ToolRegistry},
};

pub async fn build_runtime(config: &Config, fake_model: bool) -> Result<Arc<AgentRuntime>> {
    let store = Arc::new(JsonlStore::new(Config::data_dir().join("threads")).await?);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| OpsCodexError::Tool(format!("failed to build HTTP client: {error}")))?;
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(
        PromqlTool::new(client.clone(), &config.prometheus.url)?
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        DockerLogsTool::new(config.targets.allowed_containers.clone())
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        HttpGetTool::new(client, config.targets.allowed_hosts.clone())
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    if config.tools.exec {
        tools.register(Arc::new(
            ExecTool::new().with_max_output_bytes(config.runtime.max_output_bytes),
        ))?;
    }

    let model: Arc<dyn ModelProvider> = if fake_model || config.model.provider == "fake" {
        Arc::new(DemoModelProvider)
    } else if config.model.provider == "openai" {
        Arc::new(
            OpenAIResponsesProvider::new(config.api_key()?, &config.model.model)
                .with_endpoint(&config.model.endpoint),
        )
    } else {
        return Err(OpsCodexError::Model(format!(
            "unsupported model provider `{}`",
            config.model.provider
        )));
    };
    let broker = Arc::new(ApprovalBroker::new());
    Ok(Arc::new(AgentRuntime::new(
        model,
        tools,
        PolicyEngine::new(broker),
        store,
        RuntimeConfig::from(&config.runtime),
    )))
}
