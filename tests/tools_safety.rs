use std::sync::Arc;

use opscodex::{
    policy::{ApprovalBroker, PolicyDecision, PolicyEngine},
    tools::{
        DockerLogsTool, ExecTool, FakeTool, HttpGetTool, PromqlTool, Tool, ToolRegistry, ToolRisk,
    },
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

async fn serve_once(
    status: &str,
    content_type: &str,
    body: &str,
) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = oneshot::channel();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let _ = request_sender.send(String::from_utf8(request).unwrap());
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (format!("http://{address}"), request_receiver)
}

#[tokio::test]
async fn registry_rejects_duplicate_names_and_exec_requires_approval() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(FakeTool::safe("inspect", json!({"ok": true}))))
        .unwrap();

    let duplicate = registry.register(Arc::new(FakeTool::safe("inspect", json!({"ok": false}))));

    assert!(duplicate.is_err());
    assert_eq!(ExecTool::new().risk(), ToolRisk::Ask);
    let policy = PolicyEngine::new(Arc::new(ApprovalBroker::new()));
    assert_eq!(policy.decision_for(ToolRisk::Safe), PolicyDecision::Allow);
    assert_eq!(policy.decision_for(ToolRisk::Ask), PolicyDecision::Ask);
    assert_eq!(
        policy.decision_for(ToolRisk::Forbidden),
        PolicyDecision::Deny
    );
}

#[tokio::test]
async fn approval_ids_resolve_the_matching_waiter_once() {
    let broker = ApprovalBroker::new();
    let (approval_id, receiver) = broker.request("exec", json!({"command": "uptime"}));

    broker.resolve(&approval_id, true).unwrap();

    assert!(receiver.await.unwrap());
    assert!(broker.resolve(&approval_id, false).is_err());
}

#[tokio::test]
async fn registry_executes_safe_fake_tool_and_exposes_schema() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(FakeTool::safe(
            "inspect",
            json!({"health": "degraded"}),
        )))
        .unwrap();

    let output = registry
        .execute("inspect", json!({}), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(registry.risk("inspect").unwrap(), ToolRisk::Safe);
    assert_eq!(registry.schemas()[0].name, "inspect");
    assert_eq!(output.content, json!({"health": "degraded"}));
}

#[tokio::test]
async fn read_only_tools_reject_targets_outside_their_allowlists() {
    let client = reqwest::Client::new();
    let http = HttpGetTool::new(client, ["order-service"]);
    let docker = DockerLogsTool::new(["order-service"]);

    let credentialed_url = http
        .execute(
            json!({"url": "http://user:secret@order-service/health"}),
            CancellationToken::new(),
        )
        .await;
    let forbidden_host = http
        .execute(
            json!({"url": "http://metadata.internal/latest"}),
            CancellationToken::new(),
        )
        .await;
    let forbidden_container = docker
        .execute(
            json!({"container": "production-db", "since": "10m", "tail": 10}),
            CancellationToken::new(),
        )
        .await;

    assert!(credentialed_url.is_err());
    assert!(forbidden_host.is_err());
    assert!(forbidden_container.is_err());
}

#[tokio::test]
async fn promql_uses_the_instant_query_endpoint_and_preserves_json() {
    let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
    let (base_url, request) = serve_once("200 OK", "application/json", body).await;
    let tool = PromqlTool::new(reqwest::Client::new(), &base_url).unwrap();

    let output = tool
        .execute(
            json!({"query": "up{service=\"orders\"}"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let request = request.await.unwrap();
    let target = request.lines().next().unwrap().split(' ').nth(1).unwrap();
    let target = url::Url::parse(&format!("{base_url}{target}")).unwrap();

    assert_eq!(target.path(), "/api/v1/query");
    assert_eq!(
        target
            .query_pairs()
            .find(|(name, _)| name == "query")
            .unwrap()
            .1,
        "up{service=\"orders\"}"
    );
    assert_eq!(output.content["status"], "success");
    assert_eq!(output.evidence.source, "prometheus");
}

#[tokio::test]
async fn http_get_returns_error_status_bodies_as_diagnostic_evidence() {
    let (base_url, request) = serve_once(
        "503 Service Unavailable",
        "application/json",
        r#"{"status":"degraded"}"#,
    )
    .await;
    let host = url::Url::parse(&base_url)
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned();
    let tool = HttpGetTool::new(reqwest::Client::new(), [host]);

    let output = tool
        .execute(
            json!({"url": format!("{base_url}/health")}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(request.await.unwrap().starts_with("GET /health HTTP/1.1"));
    assert_eq!(output.content["status"], 503);
    assert_eq!(output.content["body"]["status"], "degraded");
}

#[tokio::test]
async fn http_get_bounds_large_response_bodies() {
    let body = "x".repeat(256);
    let (base_url, _) = serve_once("200 OK", "text/plain", &body).await;
    let host = url::Url::parse(&base_url)
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned();
    let output = HttpGetTool::new(reqwest::Client::new(), [host])
        .with_max_output_bytes(64)
        .execute(
            json!({"url": format!("{base_url}/large")}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let text = output.content["body"].as_str().unwrap();
    assert!(output.evidence.truncated);
    assert!(text.len() <= 64);
    assert!(text.ends_with("[output truncated]"));
}

#[tokio::test]
async fn promql_bounds_large_response_bodies() {
    let body = format!(r#"{{"status":"success","data":"{}"}}"#, "x".repeat(256));
    let (base_url, _) = serve_once("200 OK", "application/json", &body).await;
    let output = PromqlTool::new(reqwest::Client::new(), &base_url)
        .unwrap()
        .with_max_output_bytes(64)
        .execute(json!({"query": "up"}), CancellationToken::new())
        .await
        .unwrap();

    assert!(output.evidence.truncated);
    assert_eq!(output.content["truncated"], true);
    assert!(
        output.content["raw"]
            .as_str()
            .unwrap()
            .ends_with("[output truncated]")
    );
}

#[tokio::test]
async fn exec_marks_and_bounds_truncated_output() {
    let output = ExecTool::new()
        .execute(
            json!({"command": "printf '%070000d' 0"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let text = output.content["output"].as_str().unwrap();

    assert!(output.evidence.truncated);
    assert!(text.ends_with("[output truncated]"));
    assert!(text.len() <= 64 * 1024);
}

#[tokio::test]
async fn exec_nonzero_exit_is_reported_as_a_tool_error() {
    let error = ExecTool::new()
        .execute(
            json!({"command": "printf failed; exit 7"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("exit status: 7"));
    assert!(error.to_string().contains("failed"));
}

#[cfg(unix)]
#[tokio::test]
async fn docker_logs_uses_bounded_output_for_an_allowlisted_container() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let fake_docker = directory.path().join("docker");
    tokio::fs::write(&fake_docker, "#!/bin/sh\nprintf '%070000d' 0\n")
        .await
        .unwrap();
    tokio::fs::set_permissions(&fake_docker, std::fs::Permissions::from_mode(0o700))
        .await
        .unwrap();
    let tool = DockerLogsTool::new(["order-service"]).with_binary(fake_docker);

    let output = tool
        .execute(
            json!({"container": "order-service", "since": "5m", "tail": 100}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let logs = output.content["logs"].as_str().unwrap();

    assert!(output.evidence.truncated);
    assert!(logs.ends_with("[output truncated]"));
    assert!(logs.len() <= 64 * 1024);
}
