use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use opscodex::tools::{
    K8sEventsTool, K8sGetTool, K8sLogsTool, KubernetesClient, KubernetesPolicy, Tool,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn k8s_get_returns_sanitized_workload_evidence() -> anyhow::Result<()> {
    let app = Router::new().route("/api/v1/namespaces/{namespace}/pods/{name}", get(pod));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = KubernetesClient::new(
        reqwest::Client::new(),
        format!("http://{address}"),
        policy(),
    )?;
    let tool = K8sGetTool::new(Arc::new(client));
    let output = tool
        .execute(
            json!({"kind": "Pod", "namespace": "checkout", "name": "order-service"}),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(output.evidence.source, "kubernetes");
    assert_eq!(output.content["cluster"], "staging");
    assert!(output.content["object"]["data"].is_null());
    assert_eq!(
        output.content["object"]["metadata"]["name"],
        "order-service"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn k8s_get_rejects_namespace_escape_and_secrets() -> anyhow::Result<()> {
    let client = KubernetesClient::new(reqwest::Client::new(), "http://127.0.0.1:9", policy())?;
    let tool = K8sGetTool::new(Arc::new(client));
    let escaped = tool
        .execute(
            json!({"kind": "Pod", "namespace": "kube-system", "name": "coredns"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(escaped.to_string().contains("allowlist"));
    let secret = tool
        .execute(
            json!({"kind": "Secret", "namespace": "checkout", "name": "db"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(secret.to_string().contains("not allowlisted"));
    Ok(())
}

#[tokio::test]
async fn k8s_events_and_logs_stay_in_scope() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/v1/namespaces/{namespace}/events", get(events))
        .route("/api/v1/namespaces/{namespace}/pods/{name}/log", get(logs));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = Arc::new(KubernetesClient::new(
        reqwest::Client::new(),
        format!("http://{address}"),
        policy(),
    )?);
    let events_tool = K8sEventsTool::new(client.clone());
    let logs_tool = K8sLogsTool::new(client);
    let events = events_tool
        .execute(
            json!({
                "namespace": "checkout",
                "involved_kind": "Pod",
                "involved_name": "order-service"
            }),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(events.content["count"], 1);
    let logs = logs_tool
        .execute(
            json!({"namespace": "checkout", "pod": "order-service", "tail_lines": 20}),
            CancellationToken::new(),
        )
        .await?;
    assert!(
        logs.content["logs"]
            .as_str()
            .unwrap()
            .contains("pool exhausted")
    );
    assert!(!logs.content.to_string().contains("Bearer"));
    server.abort();
    Ok(())
}

#[tokio::test]
async fn k8s_get_maps_rbac_denied_to_auth_error() -> anyhow::Result<()> {
    let app = Router::new().route(
        "/api/v1/namespaces/{namespace}/pods/{name}",
        get(|| async { StatusCode::FORBIDDEN }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = KubernetesClient::new(
        reqwest::Client::new(),
        format!("http://{address}"),
        policy(),
    )?;
    let tool = K8sGetTool::new(Arc::new(client));
    let error = tool
        .execute(
            json!({"kind": "Pod", "namespace": "checkout", "name": "order-service"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("403")
            || error.to_string().to_ascii_lowercase().contains("auth")
    );
    server.abort();
    Ok(())
}

#[test]
fn kubernetes_client_rejects_write_verbs() {
    assert!(KubernetesClient::rejects_write_verb("patch"));
    assert!(KubernetesClient::rejects_write_verb("delete"));
    assert!(!KubernetesClient::rejects_write_verb("get"));
}

fn policy() -> KubernetesPolicy {
    KubernetesPolicy {
        cluster_alias: "staging".into(),
        allowed_namespaces: vec!["checkout".into()],
        allowed_kinds: vec![
            "Pod".into(),
            "Deployment".into(),
            "Event".into(),
            "Service".into(),
        ],
    }
}

async fn pod(Path((_, name)): Path<(String, String)>) -> Json<Value> {
    Json(json!({
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": "checkout",
            "ownerReferences": [{"kind": "ReplicaSet", "name": "order-service-abc"}]
        },
        "data": {"token": "should-not-leak"},
        "status": {"phase": "Running"}
    }))
}

async fn events(
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let _ = (query, headers);
    (
        StatusCode::OK,
        Json(json!({
            "items": [{
                "reason": "Unhealthy",
                "message": "Readiness probe failed",
                "involvedObject": {"kind": "Pod", "name": "order-service"}
            }]
        })),
    )
}

async fn logs() -> &'static str {
    "2026-08-16T00:00:01Z database pool exhausted"
}
