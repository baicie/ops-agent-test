use std::sync::Arc;

use async_trait::async_trait;
use opscodex::{
    evidence::{ClaimKind, parse_diagnosis, validate_diagnosis},
    model::{ModelEventSink, ModelItem, ModelOutput, ModelProvider, ModelRequest, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{
        AgentRuntime, IncidentContext, IncidentSource, RuntimeConfig, RuntimeEvent, ThreadId,
        TurnId, TurnInput, WorkspaceId,
    },
    store::JsonlStore,
    tools::{FakeTool, ToolRegistry, TopologyQueryTool},
    topology::project_topology,
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

struct ErrorSurgeModel;

#[async_trait]
impl ModelProvider for ErrorSurgeModel {
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
                json!({"query": "sum(rate(http_requests_total{status=~\"5..\"}[1m]))"}),
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
                    "summary": "Checkout 5xx surged after an alert; logs show handler panics.",
                    "claims": [
                        {
                            "kind": "observed",
                            "statement": "Error rate and logs confirm a service error surge.",
                            "evidence_ids": evidence_ids,
                            "confidence": "high"
                        }
                    ],
                    "recommended_actions": ["Page the service owner and inspect recent deploys."],
                    "limitations": ["Alert context is a hint, not evidence."]
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
async fn error_surge_uses_alert_metrics_and_logs_without_treating_alert_as_evidence()
-> anyhow::Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "promql_query",
        json!({"status": "success", "data": {"result": [{"value": ["now", "0.42"]}]}}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "log_query",
        json!({"status": "success", "data": {"result": [{"values": [["1", "panic: nil pointer in checkout handler"]]}]}}),
    )))?;
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("severity".into(), "critical".into());
    let (recorded, diagnosis) = scripted_investigation(
        ErrorSurgeModel,
        tools,
        TurnInput {
            content: "checkout is erroring".into(),
            incident_context: Some(IncidentContext {
                service: Some("order-service".into()),
                source: Some(IncidentSource {
                    kind: "alert".into(),
                    fingerprint: Some("checkout-5xx".into()),
                }),
                labels,
                ..IncidentContext::default()
            }),
        },
    )
    .await?;
    assert!(recorded.iter().any(|envelope| matches!(
        &envelope.event,
        RuntimeEvent::UserMessage {
            incident_context: Some(context),
            ..
        } if context.service.as_deref() == Some("order-service")
    )));
    let evidence_tools: Vec<_> = recorded
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted { tool, success, .. } if *success => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert!(evidence_tools.contains(&"promql_query"));
    assert!(evidence_tools.contains(&"log_query"));
    assert!(!evidence_tools.contains(&"alert"));
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Observed && claim.evidence_ids.len() >= 2)
    );
    Ok(())
}

struct LatencyRegressionModel;

#[async_trait]
impl ModelProvider for LatencyRegressionModel {
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
                json!({"query": "histogram_quantile(0.95, rate(http_request_duration_seconds_bucket[5m]))"}),
            ),
            1 => (
                "traces",
                "trace_search",
                json!({
                    "service": "order-service",
                    "start": "2026-08-16T00:00:00Z",
                    "end": "2026-08-16T00:05:00Z"
                }),
            ),
            2 => ("topology", "topology_query", json!({"depth": 2})),
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
                    "summary": "Checkout P95 latency regressed on the order-service hop.",
                    "claims": [
                        {
                            "kind": "observed",
                            "statement": "Metrics, traces and topology agree the slow hop is order-service.",
                            "evidence_ids": evidence_ids,
                            "confidence": "high"
                        }
                    ],
                    "recommended_actions": ["Inspect the slow span and recent deploys."],
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
async fn latency_regression_links_metrics_traces_and_topology() -> anyhow::Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "promql_query",
        json!({"status": "success", "data": {"result": [{"value": ["now", "2.4"]}]}}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "trace_search",
        json!({
            "traces": [{
                "traceID": "abc",
                "durationMs": 2400,
                "client": "checkout-ui",
                "server": "order-service",
                "rootServiceName": "checkout-ui"
            }]
        }),
    )))?;
    tools.register(Arc::new(TopologyQueryTool))?;
    let (recorded, diagnosis) = scripted_investigation_with_config(
        LatencyRegressionModel,
        tools,
        TurnInput {
            content: "checkout is slow".into(),
            incident_context: None,
        },
        RuntimeConfig {
            inline_artifact_bytes: 1,
            ..RuntimeConfig::default()
        },
    )
    .await?;
    let tools_used: Vec<_> = recorded
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted { tool, success, .. } if *success => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert!(tools_used.contains(&"promql_query"));
    assert!(tools_used.contains(&"trace_search"));
    assert!(tools_used.contains(&"topology_query"));
    let graph = project_topology(&WorkspaceId::default(), &recorded);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| { edge.from.contains("checkout-ui") && edge.to.contains("order-service") }),
        "artifact-backed trace output must retain a bounded topology projection: {graph:?}"
    );
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Observed && claim.evidence_ids.len() >= 3)
    );
    Ok(())
}

struct DownstreamDependencyModel;

#[async_trait]
impl ModelProvider for DownstreamDependencyModel {
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
                "traces",
                "trace_search",
                json!({
                    "service": "order-service",
                    "start": "2026-08-16T00:00:00Z",
                    "end": "2026-08-16T00:05:00Z"
                }),
            ),
            1 => ("topology", "topology_query", json!({"depth": 2})),
            2 => (
                "dependency-health",
                "http_get",
                json!({"url": "http://payment-service/health"}),
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
                    "summary": "order-service is blocked by a failing payment-service dependency.",
                    "claims": [
                        {
                            "kind": "observed",
                            "statement": "Traces and topology show checkout calling a failing payment-service.",
                            "evidence_ids": evidence_ids,
                            "confidence": "high"
                        }
                    ],
                    "recommended_actions": ["Check payment-service health before changing checkout."],
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
async fn downstream_dependency_failure_links_trace_and_topology() -> anyhow::Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "trace_search",
        json!({
            "traces": [{
                "traceID": "dep",
                "durationMs": 3100,
                "client": "order-service",
                "server": "payment-service",
                "error": true
            }]
        }),
    )))?;
    tools.register(Arc::new(TopologyQueryTool))?;
    tools.register(Arc::new(FakeTool::safe(
        "http_get",
        json!({"status": 503, "service": "payment-service", "health": "unavailable"}),
    )))?;
    let (recorded, diagnosis) = scripted_investigation(
        DownstreamDependencyModel,
        tools,
        TurnInput {
            content: "checkout cannot complete payments".into(),
            incident_context: None,
        },
    )
    .await?;
    let tools_used: Vec<_> = recorded
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted { tool, success, .. } if *success => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert!(tools_used.contains(&"trace_search"));
    assert!(tools_used.contains(&"topology_query"));
    assert!(tools_used.contains(&"http_get"));
    let topology = recorded.iter().find_map(|envelope| match &envelope.event {
        RuntimeEvent::ToolCompleted {
            tool,
            output,
            success,
            ..
        } if *success && tool == "topology_query" => Some(output.clone()),
        _ => None,
    });
    let topology = topology.expect("topology projection");
    let payload = topology.get("content").unwrap_or(&topology);
    let edges = payload["edges"].as_array().cloned().unwrap_or_default();
    assert!(
        !edges.is_empty(),
        "topology_query should project trace edges: {topology}"
    );
    assert!(
        edges.iter().any(|edge| {
            edge["from"]
                .as_str()
                .is_some_and(|value| value.contains("order-service"))
                && edge["to"]
                    .as_str()
                    .is_some_and(|value| value.contains("payment-service"))
        }),
        "{topology}"
    );
    assert!(
        diagnosis
            .claims
            .iter()
            .any(|claim| claim.kind == ClaimKind::Observed && claim.evidence_ids.len() >= 3)
    );
    Ok(())
}

async fn scripted_investigation(
    model: impl ModelProvider + 'static,
    tools: ToolRegistry,
    input: TurnInput,
) -> anyhow::Result<(
    Vec<opscodex::runtime::EventEnvelope>,
    opscodex::evidence::Diagnosis,
)> {
    scripted_investigation_with_config(model, tools, input, RuntimeConfig::default()).await
}

async fn scripted_investigation_with_config(
    model: impl ModelProvider + 'static,
    tools: ToolRegistry,
    input: TurnInput,
    config: RuntimeConfig,
) -> anyhow::Result<(
    Vec<opscodex::runtime::EventEnvelope>,
    opscodex::evidence::Diagnosis,
)> {
    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(model),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        config,
    );
    let (events, _) = broadcast::channel(64);
    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            input,
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
    Ok((recorded, diagnosis))
}
