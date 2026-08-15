use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Response, StatusCode, header},
    routing::post,
};
use opscodex::model::{
    ModelEvent, ModelItem, ModelProvider, ModelRequest, OpenAIResponsesProvider, ToolSchema,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn responses_provider_streams_text_and_decodes_function_calls() -> anyhow::Result<()> {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/v1/responses", post(fake_responses))
        .with_state(captured.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let provider = OpenAIResponsesProvider::new("test-key", "gpt-test")
        .with_endpoint(format!("http://{address}/v1/responses"));
    let (sink, mut deltas) = mpsc::unbounded_channel();
    let response = provider
        .complete(
            ModelRequest {
                instructions: "Gather evidence.".into(),
                input: vec![ModelItem::UserMessage {
                    content: "Check health".into(),
                }],
                tools: vec![ToolSchema {
                    name: "http_get".into(),
                    description: "Fetch a health endpoint".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"url": {"type": "string"}},
                        "required": ["url"],
                        "additionalProperties": false
                    }),
                }],
            },
            sink,
            CancellationToken::new(),
        )
        .await?;

    assert_eq!(
        deltas.recv().await,
        Some(ModelEvent::MessageDelta("Checking ".into()))
    );
    assert_eq!(
        deltas.recv().await,
        Some(ModelEvent::MessageDelta("health".into()))
    );
    assert_eq!(response.response_id.as_deref(), Some("resp_test"));
    assert_eq!(response.usage.total_tokens, 16);
    assert!(response.outputs.iter().any(|output| matches!(
        output,
        opscodex::model::ModelOutput::ToolCall { name, arguments, .. }
            if name == "http_get" && arguments["url"] == "http://localhost:8080/health"
    )));

    let request = captured.lock().unwrap().clone().expect("request body");
    assert_eq!(request["model"], "gpt-test");
    assert_eq!(request["stream"], true);
    assert_eq!(request["tools"][0]["type"], "function");
    assert_eq!(request["tools"][0]["name"], "http_get");
    assert!(request["tools"][0].get("strict").is_none());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn responses_provider_surfaces_nested_failure_messages() -> anyhow::Result<()> {
    let app = Router::new().route("/v1/responses", post(fake_failed_response));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let provider = OpenAIResponsesProvider::new("test-key", "gpt-test")
        .with_endpoint(format!("http://{address}/v1/responses"));
    let (sink, _) = mpsc::unbounded_channel();

    let error = provider
        .complete(
            ModelRequest {
                instructions: "Gather evidence.".into(),
                input: vec![ModelItem::UserMessage {
                    content: "Check health".into(),
                }],
                tools: Vec::new(),
            },
            sink,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("fixture model failure"));
    server.abort();
    Ok(())
}

async fn fake_responses(
    State(captured): State<Arc<Mutex<Option<Value>>>>,
    Json(body): Json<Value>,
) -> Response<Body> {
    *captured.lock().unwrap() = Some(body);
    let stream = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Checking \"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"health\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"http_get\",\"arguments\":\"{\\\"url\\\":\\\"http://localhost:8080/health\\\"}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\",\"usage\":{\"input_tokens\":10,\"output_tokens\":6,\"total_tokens\":16}}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(stream))
        .unwrap()
}

async fn fake_failed_response() -> Response<Body> {
    let stream = concat!(
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"fixture model failure\"}}}\n\n",
        "data: [DONE]\n\n"
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(stream))
        .unwrap()
}
