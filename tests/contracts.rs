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
    tools::ToolRegistry,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

fn contract_fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/contracts/api-v1.json"
    )))
    .expect("api-v1 contract fixture")
}

#[tokio::test]
async fn frozen_v1_get_routes_and_error_envelope_stay_stable() -> anyhow::Result<()> {
    let fixture = contract_fixture();
    assert_eq!(fixture["version"], "v1");
    assert_eq!(fixture["evolution"], "additive");

    let (state, _directory) = fake_state().await?;
    let app = router(state);
    for path in fixture["get_routes"].as_array().expect("get_routes") {
        let path = path.as_str().expect("path");
        let response = app.clone().oneshot(request("GET", path, None)).await?;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }

    let missing = fixture["not_found"].as_str().expect("not_found");
    let response = app.oneshot(request("GET", missing, None)).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );
    Ok(())
}

#[test]
fn frozen_event_types_match_the_runtime_contract() {
    let fixture = contract_fixture();
    let names: Vec<&str> = fixture["event_types"]
        .as_array()
        .expect("event_types")
        .iter()
        .map(|value| value.as_str().expect("event type"))
        .collect();
    assert!(names.contains(&"thread_created"));
    assert!(names.contains(&"action_updated"));
    assert!(names.contains(&"context_compacted"));
    assert_eq!(names.len(), 17);
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
