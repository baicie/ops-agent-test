use std::{collections::HashMap, sync::Arc};

use crate::{
    OpsCodexError, Result,
    config::Config,
    evidence::ArtifactStore,
    extensions::ExtensionCatalog,
    model::{DemoModelProvider, ModelProvider, OpenAIResponsesProvider},
    policy::{ApprovalBroker, PolicyEngine},
    runbook::RunbookCatalog,
    runtime::{AgentRuntime, RuntimeConfig},
    store::open_store,
    tools::{
        DockerLogsTool, ExecTool, HttpGetTool, K8sEventsTool, K8sGetTool, K8sLogsTool,
        KubernetesClient, LokiLogQueryTool, PromqlTool, RunbookReadTool, RunbookSearchTool,
        TempoTraceGetTool, TempoTraceSearchTool, ToolRegistry, TopologyQueryTool,
    },
    workspace::WorkspaceCatalog,
};

pub async fn build_runtime(config: &Config, fake_model: bool) -> Result<Arc<AgentRuntime>> {
    let data_dir = Config::data_dir();
    let store = open_store(&config.store, &data_dir).await?;
    let artifacts = Arc::new(ArtifactStore::disk(data_dir.join("artifacts")).await?);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| OpsCodexError::Tool(format!("failed to build HTTP client: {error}")))?;
    let catalog = WorkspaceCatalog::from_config(config)?;
    let mut workspace_tools = HashMap::new();
    let mut workspace_skills = HashMap::new();
    let mut extensions = ExtensionCatalog::default();
    let mut default_tools = ToolRegistry::new();
    for workspace in catalog.iter() {
        let mut tools = build_workspace_tools(config, client.clone(), workspace)?;
        let skills = extensions
            .install_into(&mut tools, config, workspace, &client)
            .await;
        if workspace.id.as_str() == "default" {
            default_tools = tools.clone();
        }
        workspace_tools.insert(workspace.id.as_str().to_owned(), tools);
        workspace_skills.insert(workspace.id.as_str().to_owned(), skills);
    }
    if default_tools.is_empty()
        && let Some((_, tools)) = workspace_tools.iter().next()
    {
        default_tools = tools.clone();
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
    let runtime = AgentRuntime::new(
        model,
        default_tools,
        PolicyEngine::new(broker),
        store,
        RuntimeConfig::from(&config.runtime).with_store_timeouts(
            std::time::Duration::from_secs(config.store.approval_ttl_seconds),
            std::time::Duration::from_secs(config.store.lease_ttl_seconds),
        ),
    )
    .with_artifacts(artifacts)
    .with_workspaces(catalog, workspace_tools)
    .with_extensions(extensions)
    .with_skills(workspace_skills, config.extensions.max_skill_context_bytes);
    runtime.recover().await?;
    Ok(Arc::new(runtime))
}

fn build_workspace_tools(
    config: &Config,
    client: reqwest::Client,
    workspace: &crate::workspace::Workspace,
) -> Result<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(
        PromqlTool::new(client.clone(), &workspace.prometheus_url)?
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        DockerLogsTool::new(workspace.allowed_containers.clone())
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        HttpGetTool::new(client.clone(), workspace.allowed_hosts.clone())
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    let loki_tenant = std::env::var(&workspace.loki_tenant_env).ok();
    tools.register(Arc::new(
        LokiLogQueryTool::new(
            client.clone(),
            &workspace.loki_url,
            &workspace.loki_tenant_header,
            loki_tenant,
            config.loki.max_range_seconds,
            config.loki.max_lines,
        )?
        .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        TempoTraceSearchTool::new(
            client.clone(),
            &workspace.tempo_url,
            config.tempo.max_range_seconds,
        )?
        .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    tools.register(Arc::new(
        TempoTraceGetTool::new(client.clone(), &workspace.tempo_url)?
            .with_max_output_bytes(config.runtime.max_output_bytes),
    ))?;
    if let Some(scope) = &workspace.kubernetes
        && let Ok(kube) = KubernetesClient::from_scope(client, scope)
    {
        let kube = Arc::new(kube);
        tools.register(Arc::new(
            K8sGetTool::new(kube.clone()).with_max_output_bytes(config.runtime.max_output_bytes),
        ))?;
        tools.register(Arc::new(
            K8sEventsTool::new(kube.clone()).with_max_output_bytes(config.runtime.max_output_bytes),
        ))?;
        tools.register(Arc::new(
            K8sLogsTool::new(kube).with_max_output_bytes(config.runtime.max_output_bytes),
        ))?;
    }
    let runbooks = Arc::new(RunbookCatalog::load(workspace.runbook_dir.as_ref())?);
    tools.register(Arc::new(RunbookSearchTool::new(runbooks.clone())))?;
    tools.register(Arc::new(RunbookReadTool::new(runbooks)))?;
    tools.register(Arc::new(TopologyQueryTool))?;
    if config.tools.exec {
        tools.register(Arc::new(
            ExecTool::new().with_max_output_bytes(config.runtime.max_output_bytes),
        ))?;
    }
    Ok(tools)
}
