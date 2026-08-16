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

#[tokio::test]
async fn extensions_and_skills_are_listed_without_secrets() -> anyhow::Result<()> {
    let (state, _directory) = fake_state().await?;
    let app = router(state);
    let extensions = app
        .clone()
        .oneshot(request("GET", "/api/v1/extensions", None))
        .await?;
    assert_eq!(extensions.status(), StatusCode::OK);
    let extensions: Value =
        serde_json::from_slice(&extensions.into_body().collect().await?.to_bytes())?;
    assert!(extensions["extensions"].as_array().unwrap().is_empty());

    let skills = app
        .oneshot(request("GET", "/api/v1/skills?workspace_id=default", None))
        .await?;
    assert_eq!(skills.status(), StatusCode::OK);
    let skills: Value = serde_json::from_slice(&skills.into_body().collect().await?.to_bytes())?;
    assert!(skills["skills"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn workspaces_are_listed_and_threads_cannot_cross_scope() -> anyhow::Result<()> {
    let config = opscodex::config::Config::from_toml(
        r#"
        [[workspaces]]
        id = "staging"
        display_name = "Staging"
        environment = "staging"
        "#,
    )?;
    let catalog = opscodex::workspace::WorkspaceCatalog::from_config(&config)?;
    let directory = TempDir::new()?;
    let store = Arc::new(JsonlStore::new(directory.path().join("threads")).await?);
    let runtime = Arc::new(
        AgentRuntime::new(
            Arc::new(FakeModelProvider::new(vec![])),
            ToolRegistry::new(),
            PolicyEngine::new(Arc::new(ApprovalBroker::new())),
            store,
            RuntimeConfig::default(),
        )
        .with_workspaces(catalog, Default::default()),
    );
    let app = router(ServerState::new(runtime));

    let listed = app
        .clone()
        .oneshot(request("GET", "/api/v1/workspaces", None))
        .await?;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = serde_json::from_slice(&listed.into_body().collect().await?.to_bytes())?;
    assert!(
        listed["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "staging")
    );

    let denied = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/threads",
            Some(json!({"workspace_id": "production"})),
        ))
        .await?;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/threads",
            Some(json!({"workspace_id": "staging"})),
        ))
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    assert_eq!(created["workspace_id"], "staging");
    let thread_id = created["id"].as_str().unwrap();

    let crossed = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/threads/{thread_id}/topology?workspace_id=production"),
            None,
        ))
        .await?;
    assert_eq!(crossed.status(), StatusCode::FORBIDDEN);
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
