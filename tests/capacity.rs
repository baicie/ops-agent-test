use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use opscodex::{
    OpsCodexError,
    evidence::ArtifactStore,
    model::{
        FakeModelProvider, ModelEventSink, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    },
    policy::{ApprovalBroker, PolicyEngine},
    runtime::{
        AgentRuntime, ContextBudget, RuntimeConfig, RuntimeEvent, ThreadId, TurnId, WorkspaceId,
    },
    store::{EventStore, SqliteStore},
    tools::ToolRegistry,
    topology::project_topology,
};
use tempfile::tempdir;
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

fn percentile_ms(mut samples: Vec<u128>, percentile: f64) -> u128 {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let rank = ((samples.len() as f64 - 1.0) * (percentile / 100.0)).round() as usize;
    samples[rank.min(samples.len() - 1)]
}

#[tokio::test]
async fn local_command_projection_p95_is_under_100ms() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = SqliteStore::open(directory.path().join("state.sqlite3")).await?;
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let mut samples = Vec::new();
    for index in 0..25 {
        let started = Instant::now();
        store
            .append(
                &thread_id,
                None,
                RuntimeEvent::user_message(format!("probe-{index}")),
            )
            .await?;
        let events = store.events_after(&thread_id, 0).await?;
        let _graph = project_topology(&WorkspaceId::default(), &events);
        let _threads = store.list_threads().await?;
        let elapsed = started.elapsed().as_millis();
        if index >= 5 {
            samples.push(elapsed);
        }
    }
    let p95 = percentile_ms(samples, 95.0);
    assert!(
        p95 < 100,
        "local append/projection p95 was {p95} ms (limit 100 ms)"
    );
    Ok(())
}

struct CountingBlockingModel {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelProvider for CountingBlockingModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> opscodex::Result<ModelResponse> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _ = self.entered.send(());
        let permit = tokio::select! {
            _ = cancellation.cancelled() => {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return Err(OpsCodexError::Cancelled);
            }
            permit = self.release.acquire() => permit.map_err(|_| OpsCodexError::Cancelled)?,
        };
        permit.forget();
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ModelResponse::new(vec![ModelOutput::Message {
            content: serde_json::json!({
                "summary": "No live evidence was required.",
                "claims": [],
                "recommended_actions": [],
                "limitations": ["Synthetic load fixture."]
            })
            .to_string(),
        }]))
    }
}

#[tokio::test]
async fn default_slot_limit_allows_four_simultaneously_active_turns() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(AgentRuntime::new(
        Arc::new(CountingBlockingModel {
            entered: entered_tx,
            release: release.clone(),
            active: active.clone(),
            peak: peak.clone(),
        }),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store.clone(),
        RuntimeConfig::default(),
    ));
    let mut tasks = Vec::new();
    for index in 0..4 {
        let store = store.clone();
        let runtime = runtime.clone();
        tasks.push(tokio::spawn(async move {
            let thread_id = ThreadId::new();
            store
                .create_thread(thread_id.clone(), WorkspaceId::default())
                .await?;
            let (events, _) = broadcast::channel(16);
            runtime
                .run_turn(
                    thread_id,
                    TurnId::new(),
                    format!("load-{index}").into(),
                    events,
                    CancellationToken::new(),
                )
                .await
        }));
    }
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("model entry channel closed"))?;
    }
    assert_eq!(active.load(Ordering::SeqCst), 4);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    release.add_permits(4);
    for task in tasks {
        task.await??;
    }
    assert_eq!(
        runtime
            .metrics()
            .turns_completed
            .load(std::sync::atomic::Ordering::Relaxed),
        4
    );
    assert_eq!(runtime.mutation_count(), 0);
    Ok(())
}

#[tokio::test]
async fn turn_input_enforces_the_hard_32_kib_boundary() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let accepted_thread = ThreadId::new();
    let rejected_thread = ThreadId::new();
    store
        .create_thread(accepted_thread.clone(), WorkspaceId::default())
        .await?;
    store
        .create_thread(rejected_thread.clone(), WorkspaceId::default())
        .await?;
    let reply = ModelResponse::new(vec![ModelOutput::Message {
        content: serde_json::json!({
            "summary": "Boundary accepted.",
            "claims": [],
            "recommended_actions": [],
            "limitations": []
        })
        .to_string(),
    }]);
    let runtime = AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![reply])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store,
        RuntimeConfig::default(),
    );
    let (events, _) = broadcast::channel(8);
    runtime
        .run_turn(
            accepted_thread,
            TurnId::new(),
            "x".repeat(32 * 1024).into(),
            events.clone(),
            CancellationToken::new(),
        )
        .await?;
    let error = runtime
        .run_turn(
            rejected_thread,
            TurnId::new(),
            "x".repeat(32 * 1024 + 1).into(),
            events,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, OpsCodexError::Protocol(_)));
    assert!(error.to_string().contains("32768 bytes"));
    Ok(())
}

#[tokio::test]
async fn artifact_quota_rejects_oversized_payloads() -> anyhow::Result<()> {
    let store = ArtifactStore::memory().with_max_bytes(32);
    let error = store.put(&[0u8; 64]).await.unwrap_err();
    assert!(error.to_string().contains("quota"));
    assert!(error.to_string().contains("32"));
    Ok(())
}

#[tokio::test]
async fn context_budget_rejects_oversized_turn_input() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = Arc::new(SqliteStore::open(directory.path().join("state.sqlite3")).await?);
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::default())
        .await?;
    let runtime = AgentRuntime::new(
        Arc::new(FakeModelProvider::new(vec![])),
        ToolRegistry::new(),
        PolicyEngine::new(Arc::new(ApprovalBroker::new())),
        store,
        RuntimeConfig {
            context: ContextBudget {
                max_bytes: 64,
                ..ContextBudget::default()
            },
            ..RuntimeConfig::default()
        },
    );
    let (events, _) = broadcast::channel(8);
    let error = runtime
        .run_turn(
            thread_id,
            TurnId::new(),
            "x".repeat(200).into(),
            events,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, OpsCodexError::Protocol(_)));
    assert!(error.to_string().contains("context budget"));
    Ok(())
}
