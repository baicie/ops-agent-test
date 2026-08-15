use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use opscodex::{
    OpsCodexError,
    model::{
        FakeModelProvider, ModelEventSink, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    },
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig, RuntimeEvent, ThreadId, TurnId},
    store::JsonlStore,
    tools::{FakeTool, ToolRegistry},
};
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn cancellation_stops_a_waiting_model_and_records_cancelled() -> anyhow::Result<()> {
    let model = Arc::new(BlockingModel::default());
    let (runtime, store, thread_id, _directory) =
        runtime_with(model.clone(), ToolRegistry::new()).await?;
    let runtime = Arc::new(runtime);
    let turn_id = TurnId::new();
    let (events, _) = broadcast::channel(32);
    let cancellation = CancellationToken::new();
    let task = {
        let runtime = runtime.clone();
        let thread_id = thread_id.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            runtime
                .run_turn(thread_id, turn_id, "diagnose".into(), events, cancellation)
                .await
        })
    };

    model.started.notified().await;
    cancellation.cancel();
    assert!(matches!(task.await?, Err(OpsCodexError::Cancelled)));
    let persisted = store.events_after(&thread_id, 0).await?;
    assert!(matches!(
        persisted.last().map(|event| &event.event),
        Some(RuntimeEvent::TurnCancelled)
    ));
    Ok(())
}

#[tokio::test]
async fn a_thread_rejects_a_second_active_turn() -> anyhow::Result<()> {
    let model = Arc::new(BlockingModel::default());
    let (runtime, _, thread_id, _directory) =
        runtime_with(model.clone(), ToolRegistry::new()).await?;
    let runtime = Arc::new(runtime);
    let (events, _) = broadcast::channel(32);
    let first_cancel = CancellationToken::new();
    let first = {
        let runtime = runtime.clone();
        let thread_id = thread_id.clone();
        let first_cancel = first_cancel.clone();
        let events = events.clone();
        tokio::spawn(async move {
            runtime
                .run_turn(
                    thread_id,
                    TurnId::new(),
                    "first".into(),
                    events,
                    first_cancel,
                )
                .await
        })
    };
    model.started.notified().await;

    let second = runtime
        .run_turn(
            thread_id,
            TurnId::new(),
            "second".into(),
            events,
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(second, Err(OpsCodexError::TurnAlreadyRunning)));

    first_cancel.cancel();
    assert!(matches!(first.await?, Err(OpsCodexError::Cancelled)));
    Ok(())
}

#[tokio::test]
async fn cancelling_while_waiting_for_global_slot_records_cancelled() -> anyhow::Result<()> {
    let model = Arc::new(BlockingModel::default());
    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let first_thread = ThreadId::new();
    let second_thread = ThreadId::new();
    store.create_thread(first_thread.clone()).await?;
    store.create_thread(second_thread.clone()).await?;
    let runtime = Arc::new(AgentRuntime::new(
        model.clone(),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig {
            max_concurrent_turns: 1,
            ..RuntimeConfig::default()
        },
    ));
    let (events, _) = broadcast::channel(32);
    let first_cancel = CancellationToken::new();
    let first = {
        let runtime = runtime.clone();
        let events = events.clone();
        let first_cancel = first_cancel.clone();
        let first_thread = first_thread.clone();
        tokio::spawn(async move {
            runtime
                .run_turn(
                    first_thread,
                    TurnId::new(),
                    "first".into(),
                    events,
                    first_cancel,
                )
                .await
        })
    };
    model.started.notified().await;

    let second_cancel = CancellationToken::new();
    let second_turn_id = TurnId::new();
    let second = {
        let runtime = runtime.clone();
        let events = events.clone();
        let second_cancel = second_cancel.clone();
        let second_thread = second_thread.clone();
        let second_turn_id = second_turn_id.clone();
        tokio::spawn(async move {
            runtime
                .run_turn(
                    second_thread,
                    second_turn_id,
                    "second".into(),
                    events,
                    second_cancel,
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    second_cancel.cancel();
    assert!(matches!(second.await?, Err(OpsCodexError::Cancelled)));
    assert!(matches!(
        store
            .events_after(&second_thread, 0)
            .await?
            .last()
            .map(|event| &event.event),
        Some(RuntimeEvent::TurnCancelled)
    ));

    first_cancel.cancel();
    assert!(matches!(first.await?, Err(OpsCodexError::Cancelled)));
    Ok(())
}

#[tokio::test]
async fn max_steps_bounds_repeated_tool_calls() -> anyhow::Result<()> {
    let model = Arc::new(FakeModelProvider::new(vec![
        tool_call("one"),
        tool_call("two"),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::safe("inspect", json!({"ok": true}))))?;
    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await?;
    let runtime = AgentRuntime::new(
        model,
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig {
            max_steps: 2,
            ..RuntimeConfig::default()
        },
    );
    let (events, _) = broadcast::channel(32);

    let result = runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            "loop".into(),
            events,
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(OpsCodexError::MaxStepsExceeded)));
    assert!(matches!(
        store
            .events_after(&thread_id, 0)
            .await?
            .last()
            .map(|event| &event.event),
        Some(RuntimeEvent::TurnFailed { .. })
    ));
    Ok(())
}

#[derive(Default)]
struct BlockingModel {
    started: Notify,
}

#[async_trait]
impl ModelProvider for BlockingModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> opscodex::Result<ModelResponse> {
        self.started.notify_one();
        cancellation.cancelled().await;
        Err(OpsCodexError::Cancelled)
    }
}

async fn runtime_with(
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
) -> anyhow::Result<(AgentRuntime, Arc<JsonlStore>, ThreadId, tempfile::TempDir)> {
    let directory = tempdir()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await?;
    let runtime = AgentRuntime::new(
        model,
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    Ok((runtime, store, thread_id, directory))
}

fn tool_call(call_id: &str) -> ModelResponse {
    ModelResponse::new(vec![ModelOutput::ToolCall {
        call_id: call_id.into(),
        name: "inspect".into(),
        arguments: json!({}),
    }])
}
