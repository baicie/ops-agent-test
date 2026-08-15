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
async fn model_tool_evidence_model_answer_closes_the_turn() -> anyhow::Result<()> {
    let model = Arc::new(FakeModelProvider::new(vec![
        ModelResponse::new(vec![ModelOutput::ToolCall {
            call_id: "call-1".into(),
            name: "inspect".into(),
            arguments: json!({"service": "order-service"}),
        }]),
        ModelResponse::new(vec![ModelOutput::Message {
            content: "Database pool exhaustion is the likely cause.".into(),
        }]),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe(
        "inspect",
        json!({"health": "degraded", "error": "database pool exhausted"}),
    )))?;

    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let runtime = AgentRuntime::new(
        model.clone(),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let thread_id = ThreadId::new();
    let turn_id = TurnId::new();
    store.create_thread(thread_id.clone()).await?;
    let (events, mut receiver) = broadcast::channel(32);

    runtime
        .run_turn(
            thread_id.clone(),
            turn_id,
            "Why is order-service failing?".into(),
            events,
            CancellationToken::new(),
        )
        .await?;

    let history = store.model_history(&thread_id, 100).await?;
    assert_eq!(model.requests().await.len(), 2);
    assert!(history.iter().any(|item| item.is_tool_result("call-1")));
    assert!(
        history
            .iter()
            .any(|item| { item.message_contains("Database pool exhaustion is the likely cause.") })
    );

    let mut emitted = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        emitted.push(event.event);
    }
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ToolStarted { .. }))
    );
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ToolCompleted { .. }))
    );
    assert!(matches!(emitted.last(), Some(RuntimeEvent::TurnCompleted)));

    Ok(())
}
