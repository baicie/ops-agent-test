use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use opscodex::{
    action::ActionStatus,
    config::Config,
    model::FakeModelProvider,
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{AgentRuntime, RemediationRuntime, RuntimeConfig, ThreadId, WorkspaceId},
    server::{ServerState, router},
    store::{EventStore, JsonlStore, SqliteStore},
    tools::{KubernetesClient, KubernetesPolicy, ToolRegistry},
    workspace::WorkspaceCatalog,
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn demo_catalog() -> WorkspaceCatalog {
    let config = Config::from_toml(
        r#"
[[workspaces]]
id = "local-demo"
allow_remediation = true
"#,
    )
    .unwrap();
    WorkspaceCatalog::from_config(&config).unwrap()
}

fn staging_catalog() -> WorkspaceCatalog {
    let config = Config::from_toml(
        r#"
[[workspaces]]
id = "staging"
allow_remediation = true
kubeconfig_env = "OPSCODEX_TEST_KUBE"
allowed_namespaces = ["checkout"]
allowed_kinds = ["Deployment", "StatefulSet"]
"#,
    )
    .unwrap();
    WorkspaceCatalog::from_config(&config).unwrap()
}

async fn runtime_with(
    enabled: bool,
    demo_url: &str,
    kube: HashMap<String, Arc<KubernetesClient>>,
    catalog: WorkspaceCatalog,
) -> anyhow::Result<(AgentRuntime, Arc<SqliteStore>, tempfile::TempDir)> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let runtime = runtime_from_store(enabled, demo_url, kube, catalog, store.clone());
    Ok((runtime, store, directory))
}

fn runtime_from_store(
    enabled: bool,
    demo_url: &str,
    kube: HashMap<String, Arc<KubernetesClient>>,
    catalog: WorkspaceCatalog,
    store: Arc<SqliteStore>,
) -> AgentRuntime {
    let remediation = RemediationRuntime::new(
        enabled,
        false,
        false,
        demo_url.to_owned(),
        reqwest::Client::new(),
        kube,
        Duration::from_secs(1800),
    );
    AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    )
    .with_workspaces(catalog, HashMap::new())
    .with_remediation(remediation)
}

async fn open_thread(store: &SqliteStore, workspace: &str) -> anyhow::Result<ThreadId> {
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::new(workspace))
        .await?;
    Ok(thread_id)
}

#[tokio::test]
async fn default_configuration_does_not_mutate() -> anyhow::Result<()> {
    let config = Config::default();
    assert!(!config.remediation.enabled);
    let (runtime, store, _directory) =
        runtime_with(false, "http://127.0.0.1:9", HashMap::new(), demo_catalog()).await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let error = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("denied"));
    assert_eq!(runtime.mutation_count(), 0);
    Ok(())
}

#[tokio::test]
async fn jsonl_store_rejects_action_writes() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = JsonlStore::new(directory.path()).await?;
    let error = store
        .put_action_plan(opscodex::action::ActionPlan {
            plan_id: opscodex::runtime::PlanId::new(),
            workspace_id: WorkspaceId::new("local-demo"),
            thread_id: ThreadId::new(),
            diagnosis_claim_ids: Vec::new(),
            actions: Vec::new(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sqlite"));
    Ok(())
}

#[tokio::test]
async fn demo_fault_reset_plan_approve_execute_verify() -> anyhow::Result<()> {
    let mode = Arc::new(Mutex::new("latency".to_owned()));
    let app = Router::new()
        .route("/health", get(demo_health))
        .route("/debug/fault/normal", post(demo_reset))
        .with_state(mode.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    assert_eq!(runtime.mutation_count(), 0);
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(action.status, ActionStatus::AwaitingApproval);
    assert_eq!(runtime.mutation_count(), 0);

    let swapped = runtime
        .approve_action(&action.action_id, "deadbeef", true)
        .await
        .unwrap_err();
    assert!(swapped.to_string().contains("hash"));

    let replayed_hash = action.request_hash.clone();
    let authorized = runtime
        .approve_action(&action.action_id, &replayed_hash, true)
        .await?;
    assert_eq!(authorized.status, ActionStatus::Authorized);
    let replay = runtime
        .approve_action(&action.action_id, &replayed_hash, true)
        .await
        .unwrap_err();
    assert!(replay.to_string().contains("consumed") || replay.to_string().contains("authorized"));
    assert_eq!(runtime.mutation_count(), 0);

    let done = runtime
        .execute_action(&authorized.action_id, CancellationToken::new())
        .await?;
    assert_eq!(done.status, ActionStatus::Succeeded);
    assert_eq!(runtime.mutation_count(), 1);
    assert_eq!(mode.lock().unwrap().as_str(), "normal");
    runtime.verify_audit_log().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn concurrent_execute_calls_claim_an_authorized_action_once() -> anyhow::Result<()> {
    let resets = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/health", get(healthy_demo))
        .route("/debug/fault/normal", post(counted_demo_reset))
        .with_state(resets.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    let authorized = runtime
        .approve_action(&action.action_id, &action.request_hash, true)
        .await?;
    let runtime = Arc::new(runtime);
    let barrier = Arc::new(Barrier::new(3));

    let first = tokio::spawn({
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        let action_id = authorized.action_id.clone();
        async move {
            barrier.wait().await;
            runtime
                .execute_action(&action_id, CancellationToken::new())
                .await
        }
    });
    let second = tokio::spawn({
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        let action_id = authorized.action_id.clone();
        async move {
            barrier.wait().await;
            runtime
                .execute_action(&action_id, CancellationToken::new())
                .await
        }
    });
    barrier.wait().await;

    let results = [first.await?, second.await?];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(resets.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.mutation_count(), 1);
    assert_eq!(
        store
            .get_action(&authorized.action_id)
            .await?
            .unwrap()
            .status,
        ActionStatus::Succeeded
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn authorized_action_expiry_is_persisted_before_execution() -> anyhow::Result<()> {
    let resets = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/health", get(healthy_demo))
        .route("/debug/fault/normal", post(counted_demo_reset))
        .with_state(resets.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    let mut authorized = runtime
        .approve_action(&action.action_id, &action.request_hash, true)
        .await?;
    authorized.expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
    store.put_action(authorized.clone()).await?;

    let error = runtime
        .execute_action(&authorized.action_id, CancellationToken::new())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("expired"));
    assert_eq!(
        store
            .get_action(&authorized.action_id)
            .await?
            .unwrap()
            .status,
        ActionStatus::Expired
    );
    assert_eq!(resets.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.mutation_count(), 0);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn runner_error_after_execution_starts_requires_reconciliation() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(healthy_demo_without_state))
        .route("/debug/fault/normal", post(failing_demo_reset));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    let authorized = runtime
        .approve_action(&action.action_id, &action.request_hash, true)
        .await?;

    let error = runtime
        .execute_action(&authorized.action_id, CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        opscodex::OpsCodexError::NeedsReconciliation(_)
    ));
    assert_eq!(
        store
            .get_action(&authorized.action_id)
            .await?
            .unwrap()
            .status,
        ActionStatus::NeedsReconciliation
    );
    assert_eq!(runtime.mutation_count(), 0);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn recovery_marks_in_flight_actions_for_reconciliation() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/health", get(healthy_demo_without_state))
        .route("/debug/fault/normal", post(successful_demo_reset));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let mut interrupted = Vec::new();
    for status in [
        ActionStatus::PreconditionCheck,
        ActionStatus::Executing,
        ActionStatus::Verifying,
    ] {
        let mut action = runtime
            .propose_action_plan(
                &thread_id,
                "demo_fault_reset",
                json!({"service": "order-service", "mode": "normal"}),
                Vec::new(),
                CancellationToken::new(),
            )
            .await?;
        action.status = status;
        action.consumed_approval = true;
        store.put_action(action.clone()).await?;
        interrupted.push(action.action_id);
    }

    drop(runtime);
    drop(store);
    let restarted_store =
        Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let restarted = runtime_from_store(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
        restarted_store.clone(),
    );
    restarted.recover().await?;

    for action_id in interrupted {
        assert_eq!(
            restarted_store
                .get_action(&action_id)
                .await?
                .unwrap()
                .status,
            ActionStatus::NeedsReconciliation
        );
    }
    server.abort();
    Ok(())
}

#[tokio::test]
async fn parameter_swap_and_expiry_and_cross_workspace_are_rejected() -> anyhow::Result<()> {
    let mode = Arc::new(Mutex::new("latency".to_owned()));
    let app = Router::new()
        .route("/health", get(demo_health))
        .route("/debug/fault/normal", post(demo_reset))
        .with_state(mode);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let mut action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    let original_hash = action.request_hash.clone();
    action.normalized_parameters = json!({"service": "payments", "mode": "normal"});
    store.put_action(action.clone()).await?;
    let swapped = runtime
        .approve_action(&action.action_id, &original_hash, true)
        .await
        .unwrap_err();
    assert!(swapped.to_string().contains("hash"));

    let default_thread = open_thread(&store, "default").await?;
    let crossed = runtime
        .propose_action_plan(
            &default_thread,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(crossed.to_string().contains("local-demo"));

    let mut expired = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    expired.expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
    store.put_action(expired.clone()).await?;
    runtime.recover().await?;
    let after = store.get_action(&expired.action_id).await?.unwrap();
    assert_eq!(after.status, ActionStatus::Expired);
    assert_eq!(runtime.mutation_count(), 0);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn kill_switch_blocks_new_change_operations() -> anyhow::Result<()> {
    let mode = Arc::new(Mutex::new("latency".to_owned()));
    let app = Router::new()
        .route("/health", get(demo_health))
        .route("/debug/fault/normal", post(demo_reset))
        .with_state(mode);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    runtime.set_kill_switch(true);
    let thread_id = open_thread(&store, "local-demo").await?;
    let error = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("denied") || error.to_string().contains("kill"));
    assert_eq!(runtime.mutation_count(), 0);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn kubernetes_scale_toctou_is_stale_and_success_verifies() -> anyhow::Result<()> {
    let workload = Arc::new(Mutex::new(FakeWorkload {
        uid: "uid-1".into(),
        resource_version: 7,
        replicas: 1,
        available: 1,
    }));
    let app = Router::new()
        .route(
            "/apis/apps/v1/namespaces/{namespace}/deployments/{name}",
            get(get_workload).patch(patch_workload),
        )
        .with_state(workload.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let kube = KubernetesClient::new(
        reqwest::Client::new(),
        format!("http://{address}"),
        KubernetesPolicy {
            cluster_alias: "staging".into(),
            allowed_namespaces: vec!["checkout".into()],
            allowed_kinds: vec!["Deployment".into(), "StatefulSet".into()],
        },
    )?;
    let mut clients = HashMap::new();
    clients.insert("staging".into(), Arc::new(kube));
    let (runtime, store, _directory) =
        runtime_with(true, "http://127.0.0.1:9", clients, staging_catalog()).await?;
    let thread_id = open_thread(&store, "staging").await?;
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "k8s_scale",
            json!({
                "kind": "Deployment",
                "namespace": "checkout",
                "name": "order-service",
                "replicas": 3
            }),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(action.status, ActionStatus::AwaitingApproval);
    assert!(
        action
            .preconditions
            .iter()
            .any(|item| item.contains("uid=uid-1"))
    );
    workload.lock().unwrap().resource_version = 99;
    let authorized = runtime
        .approve_action(&action.action_id, &action.request_hash, true)
        .await?;
    let stale = runtime
        .execute_action(&authorized.action_id, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("stale") || stale.to_string().contains("changed"));
    let stored = store.get_action(&action.action_id).await?.unwrap();
    assert_eq!(stored.status, ActionStatus::Stale);
    assert_eq!(runtime.mutation_count(), 0);

    workload.lock().unwrap().resource_version = 7;
    let fresh = runtime
        .propose_action_plan(
            &thread_id,
            "k8s_scale",
            json!({
                "kind": "Deployment",
                "namespace": "checkout",
                "name": "order-service",
                "replicas": 2
            }),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    let approved = runtime
        .approve_action(&fresh.action_id, &fresh.request_hash, true)
        .await?;
    let done = runtime
        .execute_action(&approved.action_id, CancellationToken::new())
        .await?;
    assert_eq!(done.status, ActionStatus::Succeeded);
    assert_eq!(runtime.mutation_count(), 1);
    assert!(done.rollback_spec.is_none());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn approve_api_requires_the_displayed_request_hash() -> anyhow::Result<()> {
    let mode = Arc::new(Mutex::new("latency".to_owned()));
    let app = Router::new()
        .route("/health", get(demo_health))
        .route("/debug/fault/normal", post(demo_reset))
        .with_state(mode);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (runtime, store, _directory) = runtime_with(
        true,
        &format!("http://{address}"),
        HashMap::new(),
        demo_catalog(),
    )
    .await?;
    let thread_id = open_thread(&store, "local-demo").await?;
    let action = runtime
        .propose_action_plan(
            &thread_id,
            "demo_fault_reset",
            json!({"service": "order-service", "mode": "normal"}),
            Vec::new(),
            CancellationToken::new(),
        )
        .await?;
    runtime.verify_audit_log().await?;
    let api = router(ServerState::new(Arc::new(runtime)));
    let missing_hash = api
        .oneshot(api_request(
            "POST",
            &format!("/api/v1/actions/{}/approve", action.action_id),
            Some(json!({"approved": true})),
        ))
        .await?;
    assert_eq!(missing_hash.status(), axum::http::StatusCode::BAD_REQUEST);
    server.abort();
    Ok(())
}

async fn healthy_demo(State(_resets): State<Arc<AtomicUsize>>) -> Json<Value> {
    healthy_demo_without_state().await
}

async fn counted_demo_reset(State(resets): State<Arc<AtomicUsize>>) -> Json<Value> {
    resets.fetch_add(1, Ordering::SeqCst);
    Json(json!({"mode": "normal"}))
}

async fn healthy_demo_without_state() -> Json<Value> {
    Json(json!({"status": "ok", "mode": "normal"}))
}

async fn successful_demo_reset() -> Json<Value> {
    Json(json!({"mode": "normal"}))
}

async fn failing_demo_reset() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "reset failed"})),
    )
}

async fn demo_health(State(mode): State<Arc<Mutex<String>>>) -> Json<Value> {
    let current = mode.lock().unwrap().clone();
    Json(json!({
        "status": if current == "normal" { "ok" } else { "degraded" },
        "mode": current
    }))
}

async fn demo_reset(State(mode): State<Arc<Mutex<String>>>) -> Json<Value> {
    *mode.lock().unwrap() = "normal".into();
    Json(json!({"mode": "normal"}))
}

#[derive(Clone)]
struct FakeWorkload {
    uid: String,
    resource_version: u64,
    replicas: u32,
    available: u32,
}

async fn get_workload(State(state): State<Arc<Mutex<FakeWorkload>>>) -> Json<Value> {
    let current = state.lock().unwrap();
    Json(workload_body(&current))
}

async fn patch_workload(
    Path((_namespace, _name)): Path<(String, String)>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    State(state): State<Arc<Mutex<FakeWorkload>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let dry_run = query.get("dryRun").map(String::as_str) == Some("All");
    let replicas = body
        .pointer("/spec/replicas")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    let mut current = state.lock().unwrap();
    if !dry_run {
        current.replicas = replicas;
        current.available = replicas;
        current.resource_version += 1;
    }
    Json(workload_body(&current))
}

fn workload_body(current: &FakeWorkload) -> Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "order-service",
            "namespace": "checkout",
            "uid": current.uid,
            "resourceVersion": current.resource_version.to_string()
        },
        "spec": { "replicas": current.replicas },
        "status": {
            "availableReplicas": current.available,
            "updatedReplicas": current.available,
            "readyReplicas": current.available
        }
    })
}

fn api_request(
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::http::Request<axum::body::Body> {
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            axum::body::Body::from(body.to_string())
        }
        None => axum::body::Body::empty(),
    };
    builder.body(body).unwrap()
}
