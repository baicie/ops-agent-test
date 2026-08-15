use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use opscodex::{
    OpsCodexError,
    model::{
        FakeModelProvider, ModelEventSink, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    },
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig, RuntimeEvent, ThreadId},
    server::{ServerState, router, router_with_web},
    store::JsonlStore,
    tools::{FakeTool, ToolRegistry},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

#[tokio::test]
async fn thread_endpoints_create_list_and_get_a_thread() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let app = router(state);

    let created = app
        .clone()
        .oneshot(request("POST", "/api/threads", None))
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    let thread_id = created["id"].as_str().expect("thread id");

    let listed = app
        .clone()
        .oneshot(request("GET", "/api/threads", None))
        .await?;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = serde_json::from_slice(&listed.into_body().collect().await?.to_bytes())?;
    assert_eq!(listed.as_array().map(Vec::len), Some(1));
    assert_eq!(listed[0]["id"], thread_id);

    let fetched = app
        .oneshot(request("GET", &format!("/api/threads/{thread_id}"), None))
        .await?;
    assert_eq!(fetched.status(), StatusCode::OK);
    let fetched: Value = serde_json::from_slice(&fetched.into_body().collect().await?.to_bytes())?;
    assert_eq!(fetched["id"], thread_id);
    assert_eq!(fetched["status"], "idle");
    assert_eq!(fetched["events"][0]["seq"], 1);
    assert_eq!(fetched["events"][0]["type"], "thread_created");
    Ok(())
}

#[tokio::test]
async fn active_turn_conflicts_and_interrupt_cancels_it() -> anyhow::Result<()> {
    let model = Arc::new(BlockingModel::default());
    let directory = TempDir::new()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let runtime = Arc::new(AgentRuntime::new(
        model.clone(),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    ));
    let state = ServerState::new(runtime);
    let app = router(state);
    let created = app
        .clone()
        .oneshot(request("POST", "/api/threads", None))
        .await?;
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    let thread_id = created["id"].as_str().unwrap();

    let first = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/threads/{thread_id}/turns"),
            Some(json!({"input": "diagnose"})),
        ))
        .await?;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value = serde_json::from_slice(&first.into_body().collect().await?.to_bytes())?;
    let turn_id = first["turn_id"].as_str().unwrap();
    model.started.notified().await;

    let conflict = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/threads/{thread_id}/turns"),
            Some(json!({"input": "again"})),
        ))
        .await?;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let interrupted = app
        .oneshot(request(
            "POST",
            &format!("/api/turns/{turn_id}/interrupt"),
            None,
        ))
        .await?;
    assert_eq!(interrupted.status(), StatusCode::ACCEPTED);
    wait_for_event(&store, &thread_id.parse()?, |event| {
        matches!(event, RuntimeEvent::TurnCancelled)
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn sse_replays_events_strictly_after_the_requested_sequence() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let store = state.runtime().store();
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await?;
    store
        .append(
            &thread_id,
            None,
            RuntimeEvent::UserMessage {
                content: "replayed".into(),
            },
        )
        .await?;
    let response = router(state)
        .oneshot(request(
            "GET",
            &format!("/api/threads/{thread_id}/events?after=1"),
            None,
        ))
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()?
            .starts_with("text/event-stream")
    );
    let mut body = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(
        Duration::from_secs(1),
        futures_util::StreamExt::next(&mut body),
    )
    .await?
    .expect("SSE chunk")?;
    let frame = String::from_utf8(chunk.to_vec())?;
    assert!(frame.contains("id: 2"));
    assert!(frame.contains("event: user_message"));
    assert!(frame.contains("replayed"));
    Ok(())
}

#[tokio::test]
async fn sse_reconnect_honors_last_event_id() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let store = state.runtime().store();
    let thread_id = ThreadId::new();
    store.create_thread(thread_id.clone()).await?;
    for content in ["already seen", "resume here"] {
        store
            .append(
                &thread_id,
                None,
                RuntimeEvent::UserMessage {
                    content: content.into(),
                },
            )
            .await?;
    }
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri(format!("/api/threads/{thread_id}/events?after=1"))
                .header("last-event-id", "2")
                .body(Body::empty())?,
        )
        .await?;
    let mut body = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(
        Duration::from_secs(1),
        futures_util::StreamExt::next(&mut body),
    )
    .await?
    .expect("SSE chunk")?;
    let frame = String::from_utf8(chunk.to_vec())?;

    assert!(frame.contains("id: 3"));
    assert!(frame.contains("resume here"));
    assert!(!frame.contains("already seen"));
    Ok(())
}

#[tokio::test]
async fn interrupting_an_approval_wait_removes_the_pending_request() -> anyhow::Result<()> {
    let model = Arc::new(FakeModelProvider::new(vec![ModelResponse::new(vec![
        ModelOutput::ToolCall {
            call_id: "exec-1".into(),
            name: "exec".into(),
            arguments: json!({"command": "uptime"}),
        },
    ])]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(FakeTool::ask("exec", json!({"output": "ok"}))))?;
    let broker = Arc::new(ApprovalBroker::new());
    let directory = TempDir::new()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let runtime = Arc::new(AgentRuntime::new(
        model,
        tools,
        PolicyEngine::new(broker.clone()),
        store.clone(),
        RuntimeConfig::default(),
    ));
    let app = router(ServerState::new(runtime));
    let created = app
        .clone()
        .oneshot(request("POST", "/api/threads", None))
        .await?;
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    let thread_id = created["id"].as_str().unwrap();
    let started = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/threads/{thread_id}/turns"),
            Some(json!({"input": "run diagnostics"})),
        ))
        .await?;
    let started: Value = serde_json::from_slice(&started.into_body().collect().await?.to_bytes())?;
    let turn_id = started["turn_id"].as_str().unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while broker.pending().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    app.oneshot(request(
        "POST",
        &format!("/api/turns/{turn_id}/interrupt"),
        None,
    ))
    .await?;
    wait_for_event(&store, &thread_id.parse()?, |event| {
        matches!(event, RuntimeEvent::TurnCancelled)
    })
    .await?;
    assert!(broker.pending().is_empty());
    Ok(())
}

#[tokio::test]
async fn local_api_does_not_grant_cross_origin_access() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/threads")
                .header(header::ORIGIN, "https://attacker.example")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn static_fallback_does_not_turn_unknown_api_routes_into_html() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let web = TempDir::new()?;
    std::fs::write(web.path().join("index.html"), "<main>OpsCodex</main>")?;
    let response = router_with_web(state, web.path())
        .oneshot(request("GET", "/api/not-a-route", None))
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"))
    );
    Ok(())
}

fn request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn fake_state() -> anyhow::Result<(ServerState, TempDir)> {
    let directory = TempDir::new()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let model = Arc::new(FakeModelProvider::new(vec![ModelResponse::new(vec![
        ModelOutput::Message {
            content: "done".into(),
        },
    ])]));
    let runtime = Arc::new(AgentRuntime::new(
        model,
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store,
        RuntimeConfig::default(),
    ));
    Ok((ServerState::new(runtime), directory))
}

async fn wait_for_event(
    store: &JsonlStore,
    thread_id: &ThreadId,
    predicate: impl Fn(&RuntimeEvent) -> bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if store
                .events_after(thread_id, 0)
                .await?
                .iter()
                .any(|event| predicate(&event.event))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
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
