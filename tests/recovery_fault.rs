use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use opscodex::{
    model::{FakeModelProvider, ModelOutput, ModelResponse},
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{
        AgentRuntime, ApprovalId, RuntimeConfig, RuntimeEvent, ThreadId, TurnId, TurnStatus,
        WorkspaceId,
    },
    store::{
        ApprovalStatus, CheckpointPhase, CheckpointRecord, DurableApproval, EventStore,
        PendingOperation, ResumePolicy, SqliteStore, TurnRecord, approval_request_hash,
    },
    tools::{Tool, ToolOutput, ToolRegistry, ToolRisk},
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn evidence(source: &str) -> opscodex::evidence::EvidenceMeta {
    opscodex::evidence::EvidenceMeta::new(source).with_duration_ms(1)
}

fn diagnosis(summary: &str) -> String {
    json!({
        "summary": summary,
        "claims": [],
        "recommended_actions": [],
        "limitations": ["No live evidence collected."]
    })
    .to_string()
}

struct CountingTool {
    name: String,
    risk: ToolRisk,
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
        self.risk
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
    Ok(thread_id)
}

async fn plant_crash(
    store: &dyn EventStore,
    phase: CheckpointPhase,
    pending: Option<PendingOperation>,
    resume_policy: ResumePolicy,
) -> anyhow::Result<(ThreadId, TurnId)> {
    let thread_id = fill_thread(store).await?;
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
            checkpoint_id: format!("cp-{phase:?}"),
            turn_id: turn_id.clone(),
            thread_id: thread_id.clone(),
            step: 1,
            phase,
            context_input_hash: None,
            pending_operation: pending,
            last_committed_seq: 2,
            resume_policy,
            created_at: now,
        })
        .await?;
    Ok((thread_id, turn_id))
}

fn runtime(
    store: Arc<SqliteStore>,
    tools: ToolRegistry,
    responses: Vec<ModelResponse>,
) -> AgentRuntime {
    AgentRuntime::new(
        Arc::new(FakeModelProvider::new(responses)),
        tools,
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store,
        RuntimeConfig::default(),
    )
}

fn observe_pending(turn_id: &TurnId, call_id: &str) -> PendingOperation {
    PendingOperation {
        operation_id: format!("{turn_id}:{call_id}"),
        call_id: call_id.into(),
        tool: "promql_query".into(),
        arguments: json!({"query": "up"}),
        effect: "observe".into(),
        recovery: Some("none_needed".into()),
    }
}

#[tokio::test]
async fn crash_after_durable_checkpoint_classifies_each_boundary() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let cases = [
        (
            CheckpointPhase::Queued,
            None,
            ResumePolicy::ReplayModel,
            TurnStatus::Interrupted,
            ResumePolicy::ReplayModel,
        ),
        (
            CheckpointPhase::ModelRunning,
            None,
            ResumePolicy::ReplayModel,
            TurnStatus::Interrupted,
            ResumePolicy::ReplayModel,
        ),
        (
            CheckpointPhase::WaitingApproval,
            None,
            ResumePolicy::WaitApproval,
            TurnStatus::WaitingApproval,
            ResumePolicy::WaitApproval,
        ),
        (
            CheckpointPhase::ToolCompleted,
            None,
            ResumePolicy::SkipCompletedTool,
            TurnStatus::Interrupted,
            ResumePolicy::SkipCompletedTool,
        ),
        (
            CheckpointPhase::NeedsReconciliation,
            None,
            ResumePolicy::Reconcile,
            TurnStatus::NeedsReconciliation,
            ResumePolicy::Reconcile,
        ),
    ];
    let runtime = runtime(store.clone(), ToolRegistry::new(), vec![]);
    for (phase, pending, stored_policy, status, policy) in cases {
        let (_, turn_id) = plant_crash(store.as_ref(), phase, pending, stored_policy).await?;
        let reports = runtime.recover().await?;
        let report = reports
            .iter()
            .find(|item| item.turn_id == turn_id)
            .expect("recovery report");
        assert_eq!(report.status, status, "{phase:?}");
        assert_eq!(report.resume_policy, policy, "{phase:?}");
        assert_eq!(store.get_turn(&turn_id).await?.unwrap().status, status);
    }
    Ok(())
}

#[tokio::test]
async fn completed_tool_is_not_reexecuted_after_simulated_crash() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "promql_query".into(),
        risk: ToolRisk::Safe,
        hits: hits.clone(),
    }))?;
    let (thread_id, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::ToolRunning,
        Some(observe_pending(&TurnId::new(), "call-1")),
        ResumePolicy::RetryObserve,
    )
    .await?;
    let pending = observe_pending(&turn_id, "call-1");
    store
        .put_checkpoint(CheckpointRecord {
            checkpoint_id: "cp-completed-event".into(),
            turn_id: turn_id.clone(),
            thread_id: thread_id.clone(),
            step: 1,
            phase: CheckpointPhase::ToolRunning,
            context_input_hash: None,
            pending_operation: Some(pending.clone()),
            last_committed_seq: 3,
            resume_policy: ResumePolicy::RetryObserve,
            created_at: chrono::Utc::now(),
        })
        .await?;
    store
        .append(
            &thread_id,
            Some(turn_id.clone()),
            RuntimeEvent::ToolCompleted {
                call_id: "call-1".into(),
                tool: "promql_query".into(),
                output: json!({"ok": true}),
                evidence: evidence("promql_query"),
                success: true,
            },
        )
        .await?;
    let runtime = runtime(
        store.clone(),
        tools,
        vec![ModelResponse::new(vec![ModelOutput::Message {
            content: diagnosis("pool exhausted"),
        }])],
    );
    runtime.recover().await?;
    assert_eq!(
        store.get_turn(&turn_id).await?.unwrap().status,
        TurnStatus::Interrupted
    );
    runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(16).0,
            CancellationToken::new(),
            Some("resume-completed".into()),
        )
        .await?;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn observe_retry_after_crash_runs_the_tool_once() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "promql_query".into(),
        risk: ToolRisk::Safe,
        hits: hits.clone(),
    }))?;
    let (_, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::ToolRunning,
        Some(observe_pending(&TurnId::new(), "call-obs")),
        ResumePolicy::RetryObserve,
    )
    .await?;
    let thread = store.get_turn(&turn_id).await?.unwrap().thread_id;
    store
        .put_checkpoint(CheckpointRecord {
            checkpoint_id: "cp-obs-retry".into(),
            turn_id: turn_id.clone(),
            thread_id: thread,
            step: 1,
            phase: CheckpointPhase::ToolRunning,
            context_input_hash: None,
            pending_operation: Some(observe_pending(&turn_id, "call-obs")),
            last_committed_seq: 2,
            resume_policy: ResumePolicy::RetryObserve,
            created_at: chrono::Utc::now(),
        })
        .await?;
    let runtime = runtime(
        store.clone(),
        tools,
        vec![ModelResponse::new(vec![ModelOutput::Message {
            content: diagnosis("checked"),
        }])],
    );
    runtime.recover().await?;
    runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(16).0,
            CancellationToken::new(),
            Some("resume-observe".into()),
        )
        .await?;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn expired_approval_is_not_executed_after_restart() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "exec".into(),
        risk: ToolRisk::Ask,
        hits: hits.clone(),
    }))?;
    let arguments = json!({"command": "reboot"});
    let schema_hash = tools.descriptor("exec")?.provenance.schema_hash;
    let (thread_id, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::WaitingApproval,
        Some(PendingOperation {
            operation_id: "op-exec".into(),
            call_id: "call-exec".into(),
            tool: "exec".into(),
            arguments: arguments.clone(),
            effect: "external_side_effect".into(),
            recovery: Some("none_needed".into()),
        }),
        ResumePolicy::WaitApproval,
    )
    .await?;
    store
        .put_approval(DurableApproval {
            approval_id: ApprovalId::new(),
            thread_id: Some(thread_id),
            turn_id: Some(turn_id.clone()),
            tool: "exec".into(),
            arguments: arguments.clone(),
            request_hash: approval_request_hash("exec", &arguments, Some(schema_hash.as_str())),
            schema_hash: Some(schema_hash),
            status: ApprovalStatus::Pending,
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        })
        .await?;
    let runtime = runtime(
        store.clone(),
        tools,
        vec![ModelResponse::new(vec![ModelOutput::Message {
            content: diagnosis("stopped"),
        }])],
    );
    runtime.recover().await?;
    assert_eq!(
        store.approval_for_turn(&turn_id).await?.unwrap().status,
        ApprovalStatus::Expired
    );
    runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(16).0,
            CancellationToken::new(),
            Some("resume-expired".into()),
        )
        .await?;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn durable_approved_resume_executes_once_without_asking_again() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "exec".into(),
        risk: ToolRisk::Ask,
        hits: hits.clone(),
    }))?;
    let arguments = json!({"command": "echo ok"});
    let schema_hash = tools.descriptor("exec")?.provenance.schema_hash;
    let (thread_id, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::WaitingApproval,
        Some(PendingOperation {
            operation_id: "op-approved".into(),
            call_id: "call-approved".into(),
            tool: "exec".into(),
            arguments: arguments.clone(),
            effect: "external_side_effect".into(),
            recovery: Some("none_needed".into()),
        }),
        ResumePolicy::WaitApproval,
    )
    .await?;
    store
        .put_approval(DurableApproval {
            approval_id: ApprovalId::new(),
            thread_id: Some(thread_id),
            turn_id: Some(turn_id.clone()),
            tool: "exec".into(),
            arguments: arguments.clone(),
            request_hash: approval_request_hash("exec", &arguments, Some(schema_hash.as_str())),
            schema_hash: Some(schema_hash),
            status: ApprovalStatus::Approved,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        })
        .await?;
    let runtime = runtime(
        store.clone(),
        tools,
        vec![ModelResponse::new(vec![ModelOutput::Message {
            content: diagnosis("done"),
        }])],
    );
    runtime.recover().await?;
    runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(16).0,
            CancellationToken::new(),
            Some("resume-approved".into()),
        )
        .await?;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn approval_hash_mismatch_refuses_to_execute() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let hits = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(CountingTool {
        name: "exec".into(),
        risk: ToolRisk::Ask,
        hits: hits.clone(),
    }))?;
    let arguments = json!({"command": "echo ok"});
    let schema_hash = tools.descriptor("exec")?.provenance.schema_hash;
    let (thread_id, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::WaitingApproval,
        Some(PendingOperation {
            operation_id: "op-hash".into(),
            call_id: "call-hash".into(),
            tool: "exec".into(),
            arguments: arguments.clone(),
            effect: "external_side_effect".into(),
            recovery: Some("none_needed".into()),
        }),
        ResumePolicy::WaitApproval,
    )
    .await?;
    store
        .put_approval(DurableApproval {
            approval_id: ApprovalId::new(),
            thread_id: Some(thread_id),
            turn_id: Some(turn_id.clone()),
            tool: "exec".into(),
            arguments: arguments.clone(),
            request_hash: "deadbeef".into(),
            schema_hash: Some(schema_hash),
            status: ApprovalStatus::Approved,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        })
        .await?;
    let runtime = runtime(store, tools, vec![]);
    runtime.recover().await?;
    let error = runtime
        .resume_turn(
            turn_id,
            tokio::sync::broadcast::channel(8).0,
            CancellationToken::new(),
            Some("resume-hash".into()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("request hash"));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn double_resume_with_the_same_idempotency_key_does_not_replay_the_model()
-> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let model = Arc::new(FakeModelProvider::new(vec![ModelResponse::new(vec![
        ModelOutput::Message {
            content: diagnosis("replayed once"),
        },
    ])]));
    let (_, turn_id) = plant_crash(
        store.as_ref(),
        CheckpointPhase::ModelRunning,
        None,
        ResumePolicy::ReplayModel,
    )
    .await?;
    let runtime = AgentRuntime::new(
        model.clone(),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store,
        RuntimeConfig::default(),
    );
    runtime.recover().await?;
    let events = tokio::sync::broadcast::channel(16).0;
    runtime
        .resume_turn(
            turn_id.clone(),
            events.clone(),
            CancellationToken::new(),
            Some("same-key".into()),
        )
        .await?;
    runtime
        .resume_turn(
            turn_id,
            events,
            CancellationToken::new(),
            Some("same-key".into()),
        )
        .await?;
    assert_eq!(model.requests().await.len(), 1);
    Ok(())
}
