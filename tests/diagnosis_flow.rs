use std::sync::Arc;

use opscodex::{
    model::{FakeModelProvider, ModelOutput, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig, RuntimeEvent, ThreadId, TurnId},
    store::JsonlStore,
    tools::{FakeTool, ToolRegistry},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn multi_source_incident_flow_reinjects_evidence_until_diagnosis() -> anyhow::Result<()> {
    let model = Arc::new(FakeModelProvider::new(vec![
        tool_call("metrics-errors", "promql_query", json!({"query": "5xx-rate"})),
        tool_call("metrics-latency", "promql_query", json!({"query": "p95-latency"})),
        tool_call("logs", "docker_logs", json!({"container": "order-service"})),
        tool_call(
            "health",
            "http_get",
            json!({"url": "http://order-service:8080/health"}),
        ),
        ModelResponse::new(vec![ModelOutput::Message {
            content: "Database connection pool exhaustion is the most likely root cause. Evidence: 5xx rate, high P95 latency, database pool exhausted logs, and degraded health.".into(),
        }]),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "promql_query",
        json!({"status": "success", "data": {"result": [{"value": ["now", "0.31"]}]}}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "docker_logs",
        json!({"container": "order-service", "logs": "ERROR database pool exhausted"}),
    )))?;
    tools.register(Arc::new(FakeTool::safe(
        "http_get",
        json!({"status": 200, "body": {"status": "degraded"}}),
    )))?;

    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await?;
    let runtime = AgentRuntime::new(
        model.clone(),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let (events, mut receiver) = broadcast::channel(128);

    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            "What happened to order-service?".into(),
            events,
            CancellationToken::new(),
        )
        .await?;

    let requests = model.requests().await;
    assert_eq!(requests.len(), 5);
    assert!(
        requests[1]
            .input
            .iter()
            .any(|item| item.is_tool_result("metrics-errors"))
    );
    assert!(
        requests[2]
            .input
            .iter()
            .any(|item| item.is_tool_result("metrics-latency"))
    );
    assert!(
        requests[3]
            .input
            .iter()
            .any(|item| item.is_tool_result("logs"))
    );
    assert!(
        requests[4]
            .input
            .iter()
            .any(|item| item.is_tool_result("health"))
    );

    let history = store.model_history(&thread_id, 100).await?;
    assert_eq!(
        history
            .iter()
            .filter(|item| matches!(item, opscodex::model::ModelItem::ToolResult { .. }))
            .count(),
        4
    );
    assert!(history.iter().any(|item| {
        item.message_contains("Database connection pool exhaustion is the most likely root cause")
    }));

    let mut tool_started = 0;
    let mut tool_completed = 0;
    let mut last = None;
    while let Ok(envelope) = receiver.try_recv() {
        match envelope.event {
            RuntimeEvent::ToolProposed { .. } | RuntimeEvent::ToolStarted { .. } => {
                tool_started += 1
            }
            RuntimeEvent::ToolCompleted { .. } => tool_completed += 1,
            event => last = Some(event),
        }
    }
    assert_eq!(tool_started, 4);
    assert_eq!(tool_completed, 4);
    assert!(matches!(last, Some(RuntimeEvent::TurnCompleted)));
    Ok(())
}

fn tool_call(call_id: &str, name: &str, arguments: serde_json::Value) -> ModelResponse {
    ModelResponse::new(vec![ModelOutput::ToolCall {
        call_id: call_id.into(),
        name: name.into(),
        arguments,
    }])
}
