use std::sync::Arc;

use crate::{
    OpsCodexError, Result,
    config::Config,
    evidence::ArtifactStore,
    model::{DemoModelProvider, ModelProvider, OpenAIResponsesProvider},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig},
    store::JsonlStore,
    tools::{
        DockerLogsTool, ExecTool, HttpGetTool, LokiLogQueryTool, PromqlTool, TempoTraceGetTool,
        TempoTraceSearchTool, ToolRegistry,
    },
};

pub async fn build_runtime(config: &Config, fake_model: bool) -> Result<Arc<AgentRuntime>> {
    let data_dir = Config::data_dir();
    let store = Arc::new(JsonlStore::new(data_dir.join("threads")).await?);
    let artifacts = Arc::new(ArtifactStore::disk(data_dir.join("artifacts")).await?);
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
        HttpGetTool::new(client.clone(), config.targets.allowed_hosts.clone())
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    let loki_tenant = std::env::var(&config.loki.tenant_env).ok();
    tools.register(Arc::new(
        LokiLogQueryTool::new(
            client.clone(),
            &config.loki.url,
            &config.loki.tenant_header,
            loki_tenant,
            config.loki.max_range_seconds,
            config.loki.max_lines,
        )?
        .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        TempoTraceSearchTool::new(
            client.clone(),
            &config.tempo.url,
            config.tempo.max_range_seconds,
        )?
        .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        TempoTraceGetTool::new(client, &config.tempo.url)?
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
        let mut provider = OpenAIResponsesProvider::new(config.api_key()?, &config.model.model)
            .with_endpoint(&config.model.endpoint);
        if let Some(effort) = &config.model.reasoning_effort {
            provider = provider.with_reasoning_effort(effort);
        }
        Arc::new(provider)
    } else {
        return Err(OpsCodexError::Model(format!(
            "unsupported model provider `{}`",
            config.model.provider
        )));
    };
    if config.model.reasoning_effort.is_some() && !model.capabilities().reasoning_control {
        return Err(OpsCodexError::Model(
            "model.reasoning_effort is set but the selected provider does not declare reasoning_control".into(),
        ));
    }
    let broker = Arc::new(ApprovalBroker::new());
    Ok(Arc::new(
        AgentRuntime::new(
            model,
            tools,
            PolicyEngine::new(broker),
            store,
            RuntimeConfig::from(&config.runtime),
        )
        .with_artifacts(artifacts),
    ))
}
