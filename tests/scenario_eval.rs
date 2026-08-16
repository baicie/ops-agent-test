use std::sync::Arc;

use async_trait::async_trait;
use opscodex::{
    evidence::{ClaimKind, parse_diagnosis, validate_diagnosis},
    model::{ModelEventSink, ModelItem, ModelOutput, ModelProvider, ModelRequest, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{
        AgentRuntime, RuntimeConfig, RuntimeEvent, ThreadId, TurnId, TurnInput, WorkspaceId,
    },
    store::JsonlStore,
    tools::{FakeTool, ToolRegistry},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct ScenarioModel;

#[async_trait]
impl ModelProvider for ScenarioModel {
    async fn complete(
        &self,
        request: ModelRequest,
        _sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> opscodex::Result<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(opscodex::OpsCodexError::Cancelled);
        }
        let results: Vec<_> = request
            .input
            .iter()
            .filter_map(|item| match item {
                ModelItem::ToolResult { output, call_id } => {
                    Some((call_id.clone(), output.clone()))
                }
                _ => None,
            })
            .collect();
        let next = match results.len() {
            0 => (
                "metrics",
                "promql_query",
                json!({"query": "rate(http_requests_total[5m])"}),
            ),
            1 => (
                "logs",
                "log_query",
                json!({
                    "query": "{service=\"order-service\"}",
                    "start": "2026-08-16T00:00:00Z",
                    "end": "2026-08-16T00:05:00Z"
                }),
            ),
            2 => (
                "traces",
                "trace_search",
                json!({
                    "service": "order-service",
                    "start": "2026-08-16T00:00:00Z",
                    "end": "2026-08-16T00:05:00Z"
                }),
            ),
            _ => {
                let evidence_ids: Vec<_> = results
                    .iter()
                    .filter_map(|(_, output)| {
                        output
                            .get("evidence_id")
                            .and_then(serde_json::Value::as_str)
                    })
                    .collect();
                let diagnosis = json!({
                    "summary": "Database pool exhaustion caused checkout 5xx.",
                    "claims": [
                        {
                            "kind": "observed",
                            "statement": "Error rate, logs and traces all point at pool exhaustion.",
                            "evidence_ids": evidence_ids,
                            "confidence": "high"
                        },
                        {
                            "kind": "recommended",
                            "statement": "Increase the pool size after verifying downstream latency.",
                            "evidence_ids": [],
                            "confidence": "medium"
                        }
                    ],
                    "recommended_actions": ["Raise the pool cap and recheck 5xx"],
                    "limitations": []
                });
                return Ok(ModelResponse::new(vec![ModelOutput::Message {
                    content: diagnosis.to_string(),
                }]));
            }
        };
        Ok(ModelResponse::new(vec![ModelOutput::ToolCall {
            call_id: next.0.into(),
            name: next.1.into(),
            arguments: next.2,
        }]))
    }
}

#[tokio::test]
async fn multi_signal_scenario_produces_evidence_linked_diagnosis() -> anyhow::Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "promql_query",
        json!({"status": "success", "data": {"result": [{"value": ["now", "0.31"]}]}}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "log_query",
        json!({"status": "success", "data": {"result": [{"values": [["1", "database pool exhausted"]]}]}}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "trace_search",
        json!({"traces": [{"traceID": "abc", "durationMs": 2400}]}),
    )))?;

    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(ScenarioModel),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let (events, _) = broadcast::channel(64);
    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            TurnInput {
                content: "order-service 5xx".into(),
                incident_context: None,
            },
            events,
            CancellationToken::new(),
        )
        .await?;

    let recorded = store.events_after(&thread_id, 0).await?;
    let evidence: Vec<_> = recorded
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted {
                evidence, success, ..
            } if *success => Some(evidence.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(evidence.len(), 3);
    let diagnosis = recorded
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            RuntimeEvent::AssistantCompleted { content, diagnosis } => Some(
                diagnosis
                    .clone()
                    .unwrap_or_else(|| parse_diagnosis(content)),
            ),
            _ => None,
        })
        .expect("diagnosis");
    assert!(validate_diagnosis(&diagnosis, &evidence).is_empty());
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Observed && !claim.evidence_ids.is_empty())
    );
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Recommended)
    );
    Ok(())
}

struct KubernetesScenarioModel;

#[async_trait]
impl ModelProvider for KubernetesScenarioModel {
    async fn complete(
        &self,
        request: ModelRequest,
        _sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> opscodex::Result<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(opscodex::OpsCodexError::Cancelled);
        }
        let results: Vec<_> = request
            .input
            .iter()
            .filter_map(|item| match item {
                ModelItem::ToolResult { output, call_id } => {
                    Some((call_id.clone(), output.clone()))
                }
                _ => None,
            })
            .collect();
        let next = match results.len() {
            0 => (
                "workload",
                "k8s_get",
                json!({"kind": "Deployment", "namespace": "checkout", "name": "order-service"}),
            ),
            1 => (
                "events",
                "k8s_events",
                json!({
                    "namespace": "checkout",
                    "involved_kind": "Pod",
                    "involved_name": "order-service"
                }),
            ),
            2 => (
                "logs",
                "log_query",
                json!({
                    "query": "{service=\"order-service\"}",
                    "start": "2026-08-16T00:00:00Z",
                    "end": "2026-08-16T00:05:00Z"
                }),
            ),
            _ => {
                let evidence_ids: Vec<_> = results
                    .iter()
                    .filter_map(|(_, output)| {
                        output
                            .get("evidence_id")
                            .and_then(serde_json::Value::as_str)
                    })
                    .collect();
                let diagnosis = json!({
                    "summary": "order-service pods are unready because the database pool is exhausted.",
                    "claims": [
                        {
                            "kind": "observed",
                            "statement": "Workload status, Kubernetes events and logs agree on pool exhaustion.",
                            "evidence_ids": evidence_ids,
                            "confidence": "high"
                        }
                    ],
                    "recommended_actions": ["Follow the local DB pool runbook after confirming downstream latency."],
                    "limitations": []
                });
                return Ok(ModelResponse::new(vec![ModelOutput::Message {
                    content: diagnosis.to_string(),
                }]));
            }
        };
        Ok(ModelResponse::new(vec![ModelOutput::ToolCall {
            call_id: next.0.into(),
            name: next.1.into(),
            arguments: next.2,
        }]))
    }
}

#[tokio::test]
async fn kubernetes_fixture_scenario_links_workload_events_and_logs() -> anyhow::Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "k8s_get",
        json!({
            "cluster": "staging",
            "kind": "Deployment",
            "namespace": "checkout",
            "name": "order-service",
            "object": {
                "kind": "Deployment",
                "metadata": {"name": "order-service", "namespace": "checkout"},
                "status": {"unavailableReplicas": 1}
            }
        }),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "k8s_events",
        json!({
            "cluster": "staging",
            "namespace": "checkout",
            "count": 1,
            "items": [{"reason": "Unhealthy", "message": "Readiness probe failed"}]
        }),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "log_query",
        json!({"status": "success", "data": {"result": [{"values": [["1", "database pool exhausted"]]}]}}),
    )))?;

    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(KubernetesScenarioModel),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let (events, _) = broadcast::channel(64);
    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            TurnInput {
                content: "order-service pods are unready".into(),
                incident_context: None,
            },
            events,
            CancellationToken::new(),
        )
        .await?;

    let recorded = store.events_after(&thread_id, 0).await?;
    let evidence: Vec<_> = recorded
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted {
                tool,
                evidence,
                success,
                ..
            } if *success => Some((tool.as_str(), evidence.clone())),
            _ => None,
        })
        .collect();
    assert!(evidence.iter().any(|(tool, _)| *tool == "k8s_get"));
    assert!(evidence.iter().any(|(tool, _)| *tool == "k8s_events"));
    assert!(evidence.iter().any(|(tool, _)| *tool == "log_query"));
    let metas: Vec<_> = evidence.into_iter().map(|(_, meta)| meta).collect();
    let diagnosis = recorded
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            RuntimeEvent::AssistantCompleted { content, diagnosis } => Some(
                diagnosis
                    .clone()
                    .unwrap_or_else(|| parse_diagnosis(content)),
            ),
            _ => None,
        })
        .expect("diagnosis");
    assert!(validate_diagnosis(&diagnosis, &metas).is_empty());
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Observed && claim.evidence_ids.len() >= 3)
    );
    Ok(())
}

struct AbstainModel;

#[async_trait]
impl ModelProvider for AbstainModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> opscodex::Result<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(opscodex::OpsCodexError::Cancelled);
        }
        Ok(ModelResponse::new(vec![ModelOutput::Message {
            content: json!({
                "summary": "Checkout looks broken.",
                "claims": [
                    {
                        "kind": "observed",
                        "statement": "The database is the root cause.",
                        "evidence_ids": [],
                        "confidence": "high"
                    }
                ],
                "recommended_actions": ["Collect metrics, logs, and traces before naming a cause."],
                "limitations": ["No live signals were collected."]
            })
            .to_string(),
        }]))
    }
}

#[tokio::test]
async fn insufficient_evidence_abstains_instead_of_keeping_unsourced_claims() -> anyhow::Result<()>
{
    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(AbstainModel),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let (events, _) = broadcast::channel(8);
    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            TurnInput {
                content: "why is checkout failing?".into(),
                incident_context: None,
            },
            events,
            CancellationToken::new(),
        )
        .await?;
    let recorded = store.events_after(&thread_id, 0).await?;
    let diagnosis = recorded
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            RuntimeEvent::AssistantCompleted { content, diagnosis } => Some(
                diagnosis
                    .clone()
                    .unwrap_or_else(|| parse_diagnosis(content)),
            ),
            _ => None,
        })
        .expect("diagnosis");
    assert!(
        diagnosis
            .claims
            .iter()
            .all(|claim| claim.kind != ClaimKind::Observed)
    );
    assert!(
        diagnosis
            .limitations
            .iter()
            .any(|item| item.contains("insufficient") || item.contains("Abstained"))
    );
    Ok(())
}

#[tokio::test]
async fn prompt_injection_cannot_treat_exec_as_remediation() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store =
        Arc::new(opscodex::store::SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let config = opscodex::config::Config::from_toml(
        r#"
[[workspaces]]
id = "local-demo"
allow_remediation = true
"#,
    )?;
    let catalog = opscodex::workspace::WorkspaceCatalog::from_config(&config)?;
    let runtime = AgentRuntime::new(
        Arc::new(ScenarioModel),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    )
    .with_workspaces(catalog, std::collections::HashMap::new())
    .with_remediation(opscodex::runtime::RemediationRuntime::new(
        true,
        false,
        false,
        "http://127.0.0.1:9".into(),
        reqwest::Client::new(),
        std::collections::HashMap::new(),
        std::time::Duration::from_secs(1800),
    ));
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::new("local-demo"))
        .await?;
    let error = runtime
        .propose_action_plan(
            &thread_id,
            "exec",
            json!({"command": "rm -rf /"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unsupported") || error.to_string().contains("denied"));
    assert_eq!(runtime.mutation_count(), 0);
    Ok(())
}
