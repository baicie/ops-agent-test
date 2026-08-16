use axum::{
    Json, Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use opscodex::tools::{LokiLogQueryTool, TempoTraceGetTool, TempoTraceSearchTool, Tool};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn log_query_normalizes_loki_streams_and_keeps_tenant_out_of_output() -> anyhow::Result<()> {
    let app = Router::new().route("/loki/api/v1/query_range", get(loki_range));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let tool = LokiLogQueryTool::new(
        reqwest::Client::new(),
        format!("http://{address}"),
        "X-Scope-OrgID",
        Some("secret-tenant".into()),
        3600,
        50,
    )?;
    let output = tool
        .execute(
            json!({
                "query": "{service=\"order-service\"}",
                "start": "2026-08-16T00:00:00Z",
                "end": "2026-08-16T00:05:00Z",
                "limit": 10
            }),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(output.evidence.source, "loki");
    assert!(!output.content.to_string().contains("secret-tenant"));
    assert_eq!(output.content["status"], "success");
    server.abort();
    Ok(())
}

#[tokio::test]
async fn log_query_rejects_oversized_time_range() -> anyhow::Result<()> {
    let tool = LokiLogQueryTool::new(
        reqwest::Client::new(),
        "http://127.0.0.1:9",
        "X-Scope-OrgID",
        None,
        60,
        10,
    )?;
    let error = tool
        .execute(
            json!({
                "query": "{job=\"a\"}",
                "start": "2026-08-16T00:00:00Z",
                "end": "2026-08-16T01:00:00Z"
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exceeds max"));
    Ok(())
}

#[tokio::test]
async fn tempo_search_and_get_return_bounded_summaries() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/search", get(tempo_search))
        .route("/api/traces/{id}", get(tempo_get));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let search = TempoTraceSearchTool::new(client.clone(), format!("http://{address}"), 3600)?;
    let get = TempoTraceGetTool::new(client, format!("http://{address}"))?;
    let found = search
        .execute(
            json!({
                "service": "order-service",
                "start": "2026-08-16T00:00:00Z",
                "end": "2026-08-16T00:05:00Z"
            }),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(found.content["traces"][0]["traceID"], "abc123");
    let detail = get
        .execute(json!({"trace_id": "abc123"}), CancellationToken::new())
        .await?;
    assert!(detail.evidence.summary.contains("resource batch"));
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tempo_get_maps_missing_traces() -> anyhow::Result<()> {
    let app = Router::new().route("/api/traces/{id}", get(|| async { StatusCode::NOT_FOUND }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let tool = TempoTraceGetTool::new(reqwest::Client::new(), format!("http://{address}"))?;
    let error = tool
        .execute(json!({"trace_id": "deadbeef"}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"));
    server.abort();
    Ok(())
}

async fn loki_range(headers: HeaderMap, Query(query): Query<Vec<(String, String)>>) -> Json<Value> {
    assert_eq!(headers.get("X-Scope-OrgID").unwrap(), "secret-tenant");
    assert!(
        query
            .iter()
            .any(|(key, value)| key == "query" && value.contains("order-service"))
    );
    Json(json!({
        "status": "success",
        "data": {
            "resultType": "streams",
            "result": [{
                "stream": {"service": "order-service"},
                "values": [["1710000000000000000", "database pool exhausted"]]
            }]
        }
    }))
}

async fn tempo_search() -> Json<Value> {
    Json(json!({
        "traces": [{
            "traceID": "abc123",
            "rootServiceName": "order-service",
            "durationMs": 2400
        }]
    }))
}

async fn tempo_get() -> Json<Value> {
    Json(json!({
        "batches": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "order-service"}}]},
            "scopeSpans": [{"spans": [{"name": "GET /checkout", "status": {"code": 2}}]}]
        }]
    }))
}
