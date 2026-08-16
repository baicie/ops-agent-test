use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use opscodex::{
    model::{FakeModelProvider, ModelOutput, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{
        AgentRuntime, RuntimeConfig, RuntimeEvent, ThreadId, TurnId, TurnStatus, WorkspaceId,
    },
    server::{ServerState, router},
    store::{
        CheckpointPhase, CheckpointRecord, EventStore, JsonlStore, MigrateOptions,
        PendingOperation, ResumePolicy, SqliteStore, TurnRecord, event_hash,
        migrate_jsonl_to_sqlite,
    },
    tools::{Tool, ToolOutput, ToolRegistry, ToolRisk},
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn evidence(source: &str) -> opscodex::evidence::EvidenceMeta {
    opscodex::evidence::EvidenceMeta::new(source).with_duration_ms(1)
}

async fn fill_thread(store: &dyn EventStore) -> anyhow::Result<ThreadId> {
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    store
        .append(
            &thread_id,
            None,
            RuntimeEvent::user_message("Why is order-service failing?"),
        )
        .await?;
    store
        .append(&thread_id, None, RuntimeEvent::TurnStarted)
        .await?;
    Ok(thread_id)
}

#[tokio::test]
async fn sqlite_matches_jsonl_append_and_history_contract() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let jsonl = JsonlStore::new(directory.path().join("jsonl")).await?;
    let sqlite = SqliteStore::open(directory.path().join("state.sqlite3")).await?;
    for store in [&jsonl as &dyn EventStore, &sqlite as &dyn EventStore] {
        let thread_id = fill_thread(store).await?;
        let turn_id = TurnId::new();
        store
            .append(
                &thread_id,
                Some(turn_id.clone()),
                RuntimeEvent::ToolStarted {
                    call_id: "call-1".into(),
                    tool: "promql_query".into(),
                    arguments: json!({"query": "up"}),
                },
            )
            .await?;
        store
            .append(
                &thread_id,
                Some(turn_id),
                RuntimeEvent::ToolCompleted {
                    call_id: "call-1".into(),
                    tool: "promql_query".into(),
                    output: json!({"status": "success"}),
                    evidence: evidence("prometheus"),
                    success: true,
                },
            )
            .await?;
        let history = store.model_history(&thread_id, 100).await?;
        assert_eq!(history.len(), 3);
        assert_eq!(store.last_seq(&thread_id).await?, 5);
    }
    Ok(())
}

#[tokio::test]
async fn sqlite_rejects_a_second_process_lock() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("state.sqlite3");
    let _first = SqliteStore::open(&path).await?;
    let second = SqliteStore::open(&path).await;
    let error = match second {
        Ok(_) => panic!("expected the second sqlite lock to fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("another OpsCodex process"));
    Ok(())
}

#[tokio::test]
async fn jsonl_to_sqlite_round_trip_preserves_event_hash() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let jsonl_dir = directory.path().join("threads");
    let sqlite_path = directory.path().join("state.sqlite3");
    let jsonl = JsonlStore::new(&jsonl_dir).await?;
    let thread_id = fill_thread(&jsonl).await?;
    jsonl
        .append(
            &thread_id,
            None,
            RuntimeEvent::assistant_completed("pool exhausted"),
        )
        .await?;
    let original = jsonl.events_after(&thread_id, 0).await?;
    let original_hash = event_hash(&original);

    let dry = migrate_jsonl_to_sqlite(
        &jsonl_dir,
        &sqlite_path,
        MigrateOptions {
            dry_run: true,
            verify: false,
        },
    )
    .await?;
    assert_eq!(dry.events, original.len());
    assert!(jsonl_dir.join(format!("{thread_id}.jsonl")).exists());

    let migrated = migrate_jsonl_to_sqlite(
        &jsonl_dir,
        &sqlite_path,
        MigrateOptions {
            dry_run: false,
            verify: false,
        },
    )
    .await?;
    assert_eq!(migrated.hash, dry.hash);
    assert!(!jsonl_dir.join(format!("{thread_id}.jsonl")).exists());
    assert!(migrated.backup_dir.is_some());

    let sqlite = SqliteStore::open(&sqlite_path).await?;
    let imported = sqlite.events_after(&thread_id, 0).await?;
    assert_eq!(event_hash(&imported), original_hash);

    let export_path = directory.path().join("export.jsonl");
    opscodex::store::export_thread_jsonl(&sqlite, &thread_id.to_string(), &export_path).await?;
    let exported = tokio::fs::read_to_string(&export_path).await?;
    assert!(exported.contains("thread_created"));
    drop(sqlite);

    let again = migrate_jsonl_to_sqlite(
        migrated.backup_dir.as_ref().unwrap(),
        &sqlite_path,
        MigrateOptions {
            dry_run: false,
            verify: false,
        },
    )
    .await?;
    assert_eq!(again.events, original.len());
    Ok(())
}

#[tokio::test]
async fn fork_inherits_durable_history_without_active_turn_state() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = SqliteStore::open(directory.path().join("state.sqlite3")).await?;
    let thread_id = fill_thread(&store).await?;
    store
        .append(
            &thread_id,
            Some(TurnId::new()),
            RuntimeEvent::ApprovalRequired {
                approval_id: opscodex::runtime::ApprovalId::new(),
                tool: "exec".into(),
                arguments: json!({"command": "reboot"}),
            },
        )
        .await?;
    let last = store.last_seq(&thread_id).await?;
    let child = EventStore::fork_thread(&store, &thread_id, last, Some("alt path".into())).await?;
    let child_thread = store.get_thread(&child.thread_id).await?;
    assert_eq!(child_thread.parent_thread_id, Some(thread_id.clone()));
    assert_eq!(child_thread.forked_at_seq, Some(last));
    assert!(
        !child_thread
            .items
            .iter()
            .any(|item| matches!(item, opscodex::runtime::Item::Approval { .. }))
    );
    store
        .append(&thread_id, None, RuntimeEvent::user_message("parent only"))
        .await?;
    store
        .append(
            &child.thread_id,
            None,
            RuntimeEvent::user_message("child only"),
        )
        .await?;
    let parent_events = store.events_after(&thread_id, 0).await?;
    let child_events = store.events_after(&child.thread_id, 0).await?;
    assert!(parent_events.iter().any(|event| matches!(
        &event.event,
        RuntimeEvent::UserMessage { content, .. } if content == "parent only"
    )));
    assert!(!child_events.iter().any(|event| matches!(
        &event.event,
        RuntimeEvent::UserMessage { content, .. } if content == "parent only"
    )));
    Ok(())
}

#[tokio::test]
async fn recovery_marks_unknown_change_operations_for_reconciliation() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let thread_id = fill_thread(store.as_ref()).await?;
    let turn_id = TurnId::new();
    let now = chrono::Utc::now();
    store
        .upsert_turn(TurnRecord {
            id: turn_id.clone(),
            thread_id: thread_id.clone(),
            status: TurnStatus::Running,
            active_lease_id: None,
            last_checkpoint_id: None,
            created_at: now,
            updated_at: now,
        })
        .await?;
    store
        .put_checkpoint(CheckpointRecord {
            checkpoint_id: "cp-change".into(),
            turn_id: turn_id.clone(),
            thread_id: thread_id.clone(),
            step: 1,
            phase: CheckpointPhase::ToolRunning,
            context_input_hash: None,
            pending_operation: Some(PendingOperation {
                operation_id: format!("{turn_id}:call-1"),
                call_id: "call-1".into(),
                tool: "exec".into(),
                arguments: json!({"command": "reboot"}),
                effect: "external_side_effect".into(),
                recovery: Some("none_needed".into()),
            }),
            last_committed_seq: 3,
            resume_policy: ResumePolicy::Reconcile,
            created_at: now,
        })
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    );
    let reports = runtime.recover().await?;
    assert_eq!(reports[0].status, TurnStatus::NeedsReconciliation);
    assert_eq!(
        store.get_turn(&turn_id).await?.unwrap().status,
        TurnStatus::NeedsReconciliation
    );
    let err = runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(8).0,
            CancellationToken::new(),
            Some("resume-1".into()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("needs reconciliation"));
    Ok(())
}

struct CountingTool {
    name: String,
    hits: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "count executions"
    }

    fn schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        _arguments: Value,
        _cancellation: CancellationToken,
    ) -> opscodex::Result<ToolOutput> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: json!({"ok": true}),
            evidence: evidence(&self.name),
        })
    }
}

#[tokio::test]
async fn observe_tool_can_be_retried_after_interrupt() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "promql_query".into(),
        hits: hits.clone(),
    }))?;
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![
            ModelResponse::new(vec![ModelOutput::ToolCall {
                call_id: "call-1".into(),
                name: "promql_query".into(),
                arguments: json!({"query": "up"}),
            }]),
            ModelResponse::new(vec![ModelOutput::Message {
                content: json!({
                    "summary": "Checked metrics.",
                    "claims": [],
                    "recommended_actions": [],
                    "limitations": ["No live evidence collected."]
                })
                .to_string(),
            }]),
        ])),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    ));
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let turn_id = TurnId::new();
    let (events, _) = tokio::sync::broadcast::channel(32);
    runtime
        .run_turn(
            thread_id.clone(),
            turn_id.clone(),
            opscodex::runtime::TurnInput {
                content: "check".into(),
                incident_context: None,
            },
            events,
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let now = chrono::Utc::now();
    let interrupted = TurnId::new();
    store
        .upsert_turn(TurnRecord {
            id: interrupted.clone(),
            thread_id: thread_id.clone(),
            status: TurnStatus::Running,
            active_lease_id: None,
            last_checkpoint_id: None,
            created_at: now,
            updated_at: now,
        })
        .await?;
    store
        .put_checkpoint(CheckpointRecord {
            checkpoint_id: "cp-obs".into(),
            turn_id: interrupted.clone(),
            thread_id,
            step: 0,
            phase: CheckpointPhase::ToolRunning,
            context_input_hash: None,
            pending_operation: Some(PendingOperation {
                operation_id: format!("{interrupted}:call-2"),
                call_id: "call-2".into(),
                tool: "promql_query".into(),
                arguments: json!({"query": "up"}),
                effect: "observe".into(),
                recovery: Some("none_needed".into()),
            }),
            last_committed_seq: 2,
            resume_policy: ResumePolicy::RetryObserve,
            created_at: now,
        })
        .await?;
    runtime.recover().await?;
    assert_eq!(
        store.get_turn(&interrupted).await?.unwrap().status,
        TurnStatus::Interrupted
    );
    Ok(())
}

#[tokio::test]
async fn context_compaction_keeps_original_events() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let mut config = RuntimeConfig::default();
    config.context.max_items = 4;
    config.context.max_bytes = 400;
    config.context.max_tokens = 80;
    let runtime = AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![ModelResponse::new(vec![
            ModelOutput::Message {
                content: json!({
                    "summary": "Done.",
                    "claims": [],
                    "recommended_actions": [],
                    "limitations": ["No live evidence collected."]
                })
                .to_string(),
            },
        ])])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        config,
    );
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    for index in 0..12 {
        store
            .append(
                &thread_id,
                None,
                RuntimeEvent::user_message(format!(
                    "constraint {index}: keep investigating the database pool and never skip evidence ids"
                )),
            )
            .await?;
        store
            .append(
                &thread_id,
                None,
                RuntimeEvent::assistant_completed(format!("ack {index}")),
            )
            .await?;
    }
    let (events, _) = tokio::sync::broadcast::channel(64);
    runtime
        .run_turn(
            thread_id.clone(),
            TurnId::new(),
            opscodex::runtime::TurnInput {
                content: "summarize".into(),
                incident_context: None,
            },
            events,
            CancellationToken::new(),
        )
        .await?;
    let history = store.events_after(&thread_id, 0).await?;
    assert!(
        history
            .iter()
            .any(|envelope| matches!(envelope.event, RuntimeEvent::ContextCompacted { .. }))
    );
    assert!(history.len() > 20);
    let compacted = history.iter().find_map(|envelope| match &envelope.event {
        RuntimeEvent::ContextCompacted { summary, .. } => Some(summary.clone()),
        _ => None,
    });
    let summary = compacted.expect("compaction summary");
    assert!(summary.contains("User constraints:"));
    assert!(summary.contains("never skip evidence"));
    Ok(())
}

#[tokio::test]
async fn fork_and_recovery_api_expose_lineage_and_resume_contract() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("threads.sqlite3")).await?);
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    ));
    let state = ServerState::new(runtime);
    let app = router(state);
    let created = app
        .clone()
        .oneshot(request("POST", "/api/v1/threads", None))
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(&created.into_body().collect().await?.to_bytes())?;
    let thread_id = created["id"].as_str().unwrap().to_owned();
    store
        .append(
            &thread_id.parse()?,
            None,
            RuntimeEvent::user_message("fork me"),
        )
        .await?;
    let forked = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/threads/{thread_id}/forks"),
            Some(json!({"at_seq": 2, "title": "alt"})),
        ))
        .await?;
    assert_eq!(forked.status(), StatusCode::CREATED);
    let forked: Value = serde_json::from_slice(&forked.into_body().collect().await?.to_bytes())?;
    assert_eq!(forked["parent_thread_id"], thread_id);
    assert_eq!(forked["forked_at_seq"], 2);

    let missing = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/turns/00000000-0000-0000-0000-000000000001/resume")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await?;
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
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

#[tokio::test]
async fn migrate_rejects_malformed_jsonl_without_moving_files() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let jsonl_dir = directory.path().join("threads");
    let sqlite_path = directory.path().join("state.sqlite3");
    let jsonl = JsonlStore::new(&jsonl_dir).await?;
    let thread_id = fill_thread(&jsonl).await?;
    let path = jsonl_dir.join(format!("{thread_id}.jsonl"));
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await?;
    file.write_all(b"{not json}\n").await?;
    drop(file);
    let error = migrate_jsonl_to_sqlite(
        &jsonl_dir,
        &sqlite_path,
        MigrateOptions {
            dry_run: false,
            verify: false,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("malformed JSONL"));
    assert!(path.exists());
    Ok(())
}

#[tokio::test]
async fn migrate_verify_matches_imported_sqlite_hash() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let jsonl_dir = directory.path().join("threads");
    let sqlite_path = directory.path().join("state.sqlite3");
    let jsonl = JsonlStore::new(&jsonl_dir).await?;
    let thread_id = fill_thread(&jsonl).await?;
    jsonl
        .append(
            &thread_id,
            None,
            RuntimeEvent::assistant_completed("verified"),
        )
        .await?;
    let migrated = migrate_jsonl_to_sqlite(
        &jsonl_dir,
        &sqlite_path,
        MigrateOptions {
            dry_run: false,
            verify: false,
        },
    )
    .await?;
    let verified = migrate_jsonl_to_sqlite(
        migrated.backup_dir.as_ref().unwrap(),
        &sqlite_path,
        MigrateOptions {
            dry_run: false,
            verify: true,
        },
    )
    .await?;
    assert_eq!(verified.hash, migrated.hash);
    assert_eq!(verified.events, migrated.events);
    Ok(())
}

#[tokio::test]
async fn fork_from_compacted_parent_inherits_summary_not_approvals() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = SqliteStore::open(directory.path().join("state.sqlite3")).await?;
    let thread_id = fill_thread(&store).await?;
    store
        .append(
            &thread_id,
            None,
            RuntimeEvent::ContextCompacted {
                summary_id: "sum-1".into(),
                covers_seq_start: 2,
                covers_seq_end: 2,
                source_item_ids: vec!["item-1".into()],
                source_evidence_ids: vec!["ev-1".into()],
                input_hash: "abc".into(),
                model_provider: Some("local".into()),
                model: Some("deterministic-suffix".into()),
                prompt_version: Some("v0.5".into()),
                summary: "User constraints: keep evidence ids".into(),
            },
        )
        .await?;
    store
        .append(
            &thread_id,
            Some(TurnId::new()),
            RuntimeEvent::ApprovalRequired {
                approval_id: opscodex::runtime::ApprovalId::new(),
                tool: "exec".into(),
                arguments: json!({"command": "reboot"}),
            },
        )
        .await?;
    let last = store.last_seq(&thread_id).await?;
    let child =
        EventStore::fork_thread(&store, &thread_id, last, Some("compact fork".into())).await?;
    let child_events = store.events_after(&child.thread_id, 0).await?;
    assert!(child_events.iter().any(|envelope| matches!(
        &envelope.event,
        RuntimeEvent::ContextCompacted { summary, .. } if summary.contains("keep evidence ids")
    )));
    assert!(
        !child_events
            .iter()
            .any(|envelope| matches!(envelope.event, RuntimeEvent::ApprovalRequired { .. }))
    );
    let other = store
        .get_thread_in(&WorkspaceId::new("other"), &child.thread_id)
        .await
        .unwrap_err();
    assert!(other.to_string().contains("workspace"));
    Ok(())
}
