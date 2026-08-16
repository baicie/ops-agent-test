use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use opscodex::{
    model::{FakeModelProvider, ModelOutput, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RuntimeConfig},
    server::{ServerState, router},
    store::JsonlStore,
    telemetry::RuntimeMetrics,
    tools::ToolRegistry,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn v1_turn_accepts_incident_context_without_creating_evidence() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let app = router(state);
    let created = app
        .clone()
        .oneshot(request("POST", "/api/v1/threads", None))
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    let thread_id = created["id"].as_str().unwrap();

    let started = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/threads/{thread_id}/turns"),
            Some(json!({
                "input": "Investigate checkout errors",
                "incident_context": {
                    "service": "order-service",
                    "environment": "staging",
                    "labels": {"severity": "critical"}
                }
            })),
        ))
        .await?;
    assert_eq!(started.status(), StatusCode::ACCEPTED);

    let mut detail = None;
    for _ in 0..200 {
        let fetched = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/threads/{thread_id}"),
                None,
            ))
            .await?;
        let body: Value = serde_json::from_slice(&fetched.into_body().collect().await?.to_bytes())?;
        if body["events"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|event| event["type"] == "turn_completed")
        {
            detail = Some(body);
            break;
        }
        tokio::task::yield_now().await;
    }
    let detail = detail.expect("turn completed");
    let user = detail["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["type"] == "user_message")
        .unwrap();
    assert_eq!(
        user["event"]["incident_context"]["service"],
        "order-service"
    );
    assert!(
        !detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "tool_completed" && event["event"]["success"] == true)
    );
    Ok(())
}

#[tokio::test]
async fn metrics_endpoint_avoids_high_cardinality_labels() -> anyhow::Result<()> {
    let metrics = RuntimeMetrics::default();
    RuntimeMetrics::inc(&metrics.turns_started);
    let body = metrics.render_prometheus();
    assert!(body.contains("opscodex_turns_total"));
    assert!(!metrics.uses_high_cardinality_labels());
    assert!(!body.contains("thread_id="));
    Ok(())
}

#[tokio::test]
async fn legacy_routes_still_create_threads() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let response = router(state)
        .oneshot(request("POST", "/api/threads", None))
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    Ok(())
}

fn request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
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
            content: json!({
                "summary": "No tools were needed.",
                "claims": [],
                "recommended_actions": [],
                "limitations": ["No live evidence collected."]
            })
            .to_string(),
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
