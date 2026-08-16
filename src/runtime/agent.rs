use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    config::RuntimeSettings,
    evidence::{
        ArtifactStore, EvidenceIds, apply_citation_limitations, finalize_evidence,
        model_tool_output, parse_diagnosis, redact_json, validate_diagnosis,
    },
    extensions::{ExtensionCatalog, SkillCatalog},
    model::{ModelEvent, ModelOutput, ModelProvider, ModelRequest, ModelResponse},
    policy::{PolicyDecision, PolicyEngine},
    runtime::{
        ContextBudget, EventId, TurnInput, TurnStatus, classify_checkpoint, local_summary,
        pending_operation_id,
    },
    store::{
        AppendEvent, ApprovalStatus, CheckpointPhase, CheckpointRecord, DurableApproval,
        EventStore, Lease, PendingOperation, ResumePolicy, TurnRecord, approval_request_hash,
        context_input_hash,
    },
    telemetry::RuntimeMetrics,
    tools::{ToolInvocation, ToolOutput, ToolRegistry},
    workspace::WorkspaceCatalog,
};

use super::{EventEnvelope, EvidenceMeta, RuntimeEvent, ThreadId, TurnId, WorkspaceId};

pub const SYSTEM_INSTRUCTIONS: &str = r#"You are OpsCodex, an autonomous AIOps diagnostic agent.

Your job is to investigate runtime incidents using available tools.

Rules:
1. Gather evidence before reaching conclusions.
2. Prefer structured observability tools over exec.
3. Never claim evidence that was not returned by a tool.
4. Correlate metrics, logs, traces and service health when possible.
5. Do not perform destructive or mutating operations.
6. If evidence is insufficient, abstain and say what is missing.
7. Incident context is unverified. Confirm every statement with tools.
8. Keep investigating until you can provide a useful diagnosis or a clear abstain.
9. Stay inside the current Workspace. Kubernetes, topology and runbook tools are read-only references.
10. Do not change the environment. Structured ActionPlan remediation is proposed out of band; never treat exec, MCP, or custom tools as remediation.

Successful tool results include an evidence_id. Final answers MUST be a JSON object:
{
  "summary": "one paragraph",
  "claims": [
    {
      "kind": "observed" | "inferred" | "recommended",
      "statement": "...",
      "evidence_ids": ["..."],
      "confidence": "low" | "medium" | "high"
    }
  ],
  "recommended_actions": ["..."],
  "limitations": ["..."]
}
observed and inferred claims must cite evidence_id values from tool results.
"#;

const INLINE_ARTIFACT_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub max_steps: usize,
    pub max_concurrent_turns: usize,
    pub model_timeout: Duration,
    pub tool_timeout: Duration,
    pub context_items: usize,
    pub context: ContextBudget,
    pub inline_artifact_bytes: usize,
    pub approval_ttl: Duration,
    pub lease_ttl: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_concurrent_turns: 4,
            model_timeout: Duration::from_secs(120),
            tool_timeout: Duration::from_secs(30),
            context_items: 100,
            context: ContextBudget::default(),
            inline_artifact_bytes: INLINE_ARTIFACT_BYTES,
            approval_ttl: Duration::from_secs(3600),
            lease_ttl: Duration::from_secs(30),
        }
    }
}

impl From<&RuntimeSettings> for RuntimeConfig {
    fn from(settings: &RuntimeSettings) -> Self {
        let context = ContextBudget {
            max_items: settings.context_items,
            max_tokens: settings.context_max_tokens,
            max_bytes: settings.context_max_bytes,
            max_evidence: settings.context_max_evidence,
            max_tool_calls: settings.context_max_tool_calls,
            max_cost_micros: settings.context_max_cost_micros,
        };
        Self {
            max_steps: settings.max_steps,
            max_concurrent_turns: settings.max_concurrent_turns,
            model_timeout: Duration::from_secs(settings.model_timeout_seconds),
            tool_timeout: Duration::from_secs(settings.tool_timeout_seconds),
            context_items: settings.context_items,
            context,
            inline_artifact_bytes: settings.inline_artifact_bytes,
            approval_ttl: Duration::from_secs(3600),
            lease_ttl: Duration::from_secs(30),
        }
    }
}

impl RuntimeConfig {
    pub fn with_store_timeouts(mut self, approval_ttl: Duration, lease_ttl: Duration) -> Self {
        self.approval_ttl = approval_ttl;
        self.lease_ttl = lease_ttl;
        self
    }
}

enum TurnStart {
    Fresh(TurnInput),
    Resume(crate::store::RecoveryReport),
}

pub struct AgentRuntime {
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
    pub(crate) workspaces: Arc<WorkspaceCatalog>,
    workspace_tools: Arc<HashMap<String, ToolRegistry>>,
    pub(crate) policy: PolicyEngine,
    pub(crate) store: Arc<dyn EventStore>,
    artifacts: Arc<ArtifactStore>,
    metrics: Arc<RuntimeMetrics>,
    config: RuntimeConfig,
    turn_slots: Arc<Semaphore>,
    active_threads: Arc<Mutex<HashSet<ThreadId>>>,
    extensions: Arc<ExtensionCatalog>,
    workspace_skills: Arc<HashMap<String, SkillCatalog>>,
    skill_budget_bytes: usize,
    owner_id: String,
    pub(crate) remediation: Arc<crate::runtime::remediation::RemediationRuntime>,
}

impl AgentRuntime {
    pub fn new(
        model: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        policy: PolicyEngine,
        store: Arc<dyn EventStore>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            model,
            tools,
            workspaces: Arc::new(WorkspaceCatalog::default()),
            workspace_tools: Arc::new(HashMap::new()),
            policy,
            store,
            artifacts: Arc::new(ArtifactStore::memory()),
            metrics: Arc::new(RuntimeMetrics::default()),
            turn_slots: Arc::new(Semaphore::new(config.max_concurrent_turns)),
            active_threads: Arc::new(Mutex::new(HashSet::new())),
            config,
            extensions: Arc::new(ExtensionCatalog::default()),
            workspace_skills: Arc::new(HashMap::new()),
            skill_budget_bytes: 4 * 1024,
            owner_id: uuid::Uuid::now_v7().to_string(),
            remediation: Arc::new(crate::runtime::remediation::RemediationRuntime::disabled()),
        }
    }

    pub fn with_workspaces(
        mut self,
        catalog: WorkspaceCatalog,
        tools: HashMap<String, ToolRegistry>,
    ) -> Self {
        self.workspaces = Arc::new(catalog);
        self.workspace_tools = Arc::new(tools);
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<ArtifactStore>) -> Self {
        self.artifacts = artifacts;
        self
    }

    pub fn with_metrics(mut self, metrics: Arc<RuntimeMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn with_extensions(mut self, catalog: ExtensionCatalog) -> Self {
        self.extensions = Arc::new(catalog);
        self
    }

    pub fn with_skills(
        mut self,
        skills: HashMap<String, SkillCatalog>,
        skill_budget_bytes: usize,
    ) -> Self {
        self.workspace_skills = Arc::new(skills);
        self.skill_budget_bytes = skill_budget_bytes.max(128);
        self
    }

    pub fn with_remediation(
        mut self,
        remediation: crate::runtime::remediation::RemediationRuntime,
    ) -> Self {
        self.remediation = Arc::new(remediation);
        self
    }

    pub fn workspaces(&self) -> Arc<WorkspaceCatalog> {
        self.workspaces.clone()
    }

    pub fn store(&self) -> Arc<dyn EventStore> {
        self.store.clone()
    }

    pub fn artifacts(&self) -> Arc<ArtifactStore> {
        self.artifacts.clone()
    }

    pub fn metrics(&self) -> Arc<RuntimeMetrics> {
        self.metrics.clone()
    }

    pub fn policy(&self) -> &PolicyEngine {
        &self.policy
    }

    pub fn extensions(&self) -> Arc<ExtensionCatalog> {
        self.extensions.clone()
    }

    pub fn skills_for(&self, workspace: &WorkspaceId) -> Vec<crate::extensions::SkillSummary> {
        self.workspace_skills
            .get(workspace.as_str())
            .map(SkillCatalog::summaries)
            .unwrap_or_default()
    }

    pub fn skill_summaries(
        &self,
        workspace_id: Option<&str>,
    ) -> Vec<crate::extensions::SkillSummary> {
        if let Some(workspace_id) = workspace_id {
            return self.skills_for(&WorkspaceId::new(workspace_id));
        }
        let mut summaries: Vec<_> = self
            .workspace_skills
            .values()
            .flat_map(SkillCatalog::summaries)
            .collect();
        summaries.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then(left.version.cmp(&right.version))
        });
        summaries.dedup_by(|left, right| left.id == right.id && left.version == right.version);
        summaries
    }

    pub async fn recover(&self) -> Result<Vec<crate::store::RecoveryReport>> {
        let now = Utc::now();
        for mut approval in self.store.list_pending_approvals().await? {
            if approval.expires_at.is_some_and(|expires| expires <= now) {
                approval.status = ApprovalStatus::Expired;
                self.store.put_approval(approval).await?;
            }
        }
        for mut action in self.store.list_awaiting_approval_actions().await? {
            if action.expires_at <= now {
                action.status = crate::action::transition(
                    crate::action::ActionStatus::AwaitingApproval,
                    crate::action::ActionStatus::Expired,
                )?;
                action.updated_at = Utc::now();
                self.store.put_action(action.clone()).await?;
                let _ = self
                    .store
                    .append(
                        &action.thread_id,
                        None,
                        RuntimeEvent::ActionUpdated {
                            action_id: action.action_id.clone(),
                            plan_id: action.plan_id.clone(),
                            status: action.status.as_str().to_owned(),
                            tool: action.tool_id.clone(),
                            request_hash: action.request_hash.clone(),
                            review: action.review_summary(),
                        },
                    )
                    .await;
            }
        }
        let mut reports = Vec::new();
        for turn in self.store.list_open_turns().await? {
            let _ = self.store.force_release_turn_lease(&turn.id).await;
            let Some(checkpoint) = self.store.last_checkpoint(&turn.id).await? else {
                let now = Utc::now();
                self.store
                    .upsert_turn(TurnRecord {
                        id: turn.id.clone(),
                        thread_id: turn.thread_id.clone(),
                        status: TurnStatus::Interrupted,
                        active_lease_id: None,
                        last_checkpoint_id: turn.last_checkpoint_id.clone(),
                        created_at: turn.created_at,
                        updated_at: now,
                    })
                    .await?;
                continue;
            };
            let events = self.store.events_after(&turn.thread_id, 0).await?;
            let report = classify_checkpoint(&checkpoint, &events);
            self.store
                .upsert_turn(TurnRecord {
                    id: turn.id.clone(),
                    thread_id: turn.thread_id.clone(),
                    status: report.status,
                    active_lease_id: None,
                    last_checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
                    created_at: turn.created_at,
                    updated_at: Utc::now(),
                })
                .await?;
            reports.push(report);
        }
        Ok(reports)
    }

    pub async fn recovery_report(&self, turn_id: &TurnId) -> Result<crate::store::RecoveryReport> {
        let turn = self
            .store
            .get_turn(turn_id)
            .await?
            .ok_or_else(|| OpsCodexError::NotFound(format!("turn {turn_id}")))?;
        let checkpoint = self
            .store
            .last_checkpoint(turn_id)
            .await?
            .ok_or_else(|| OpsCodexError::NotFound(format!("checkpoint for turn {turn_id}")))?;
        let events = self.store.events_after(&turn.thread_id, 0).await?;
        Ok(classify_checkpoint(&checkpoint, &events))
    }

    pub async fn resume_turn(
        &self,
        turn_id: TurnId,
        events: broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
        idempotency_key: Option<String>,
    ) -> Result<crate::store::RecoveryReport> {
        let report = self.recovery_report(&turn_id).await?;
        if report.resume_policy == ResumePolicy::Reconcile {
            return Err(OpsCodexError::NeedsReconciliation(
                report.user_action.clone(),
            ));
        }
        if let Some(key) = idempotency_key {
            let payload = serde_json::to_string(&report).unwrap_or_default();
            if let Some(existing) = self.store.remember_resume(&key, &turn_id, &payload).await? {
                return serde_json::from_str(&existing).map_err(|error| {
                    OpsCodexError::Storage(format!("invalid resume payload: {error}"))
                });
            }
        }
        let thread_id = report.thread_id.clone();
        let _permit = self
            .turn_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| OpsCodexError::Cancelled)?;
        let _active = ActiveThreadGuard::enter(self.active_threads.clone(), thread_id.clone())?;
        let lease = self
            .store
            .acquire_lease(&turn_id, &thread_id, &self.owner_id, self.config.lease_ttl)
            .await?;
        let result = self
            .run_turn_inner(
                &thread_id,
                &turn_id,
                TurnStart::Resume(report.clone()),
                &events,
                cancellation,
                &lease,
            )
            .await;
        let _ = self
            .store
            .release_lease(&lease.lease_id, lease.fencing_token)
            .await;
        result?;
        Ok(report)
    }

    pub async fn resolve_approval(
        &self,
        id: &crate::runtime::ApprovalId,
        approved: bool,
    ) -> Result<()> {
        if let Some(mut durable) = self.store.get_approval(id).await? {
            if durable.status != ApprovalStatus::Pending {
                return Err(OpsCodexError::Policy(format!(
                    "approval {id} is {}",
                    durable.status.as_str()
                )));
            }
            if durable
                .expires_at
                .is_some_and(|expires| expires <= Utc::now())
            {
                durable.status = ApprovalStatus::Expired;
                self.store.put_approval(durable).await?;
                let _ = self.policy.broker().resolve(id, false);
                return Err(OpsCodexError::Policy(format!("approval {id} expired")));
            }
            durable.status = if approved {
                ApprovalStatus::Approved
            } else {
                ApprovalStatus::Rejected
            };
            self.store.put_approval(durable).await?;
            return match self.policy.broker().resolve(id, approved) {
                Ok(()) | Err(OpsCodexError::NotFound(_)) => Ok(()),
                Err(error) => Err(error),
            };
        }
        self.policy.broker().resolve(id, approved)
    }

    pub async fn fork_thread(
        &self,
        thread_id: &ThreadId,
        at_seq: u64,
        title: Option<String>,
    ) -> Result<EventEnvelope> {
        self.store.fork_thread(thread_id, at_seq, title).await
    }

    fn tools_for(&self, workspace: &WorkspaceId) -> Result<&ToolRegistry> {
        if self.workspace_tools.is_empty() {
            return Ok(&self.tools);
        }
        self.workspace_tools.get(workspace.as_str()).ok_or_else(|| {
            OpsCodexError::Policy(format!(
                "workspace `{}` is not configured",
                workspace.as_str()
            ))
        })
    }

    pub async fn run_turn(
        &self,
        thread_id: ThreadId,
        turn_id: TurnId,
        input: TurnInput,
        events: broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let turn_started = Instant::now();
        RuntimeMetrics::inc(&self.metrics.turns_started);
        let result = async {
            let _permit = tokio::select! {
                _ = cancellation.cancelled() => Err(OpsCodexError::Cancelled),
                permit = self.turn_slots.clone().acquire_owned() => {
                    permit.map_err(|_| OpsCodexError::Cancelled)
                }
            }?;
            let _active = ActiveThreadGuard::enter(self.active_threads.clone(), thread_id.clone())?;
            let lease = self
                .store
                .acquire_lease(&turn_id, &thread_id, &self.owner_id, self.config.lease_ttl)
                .await?;
            let now = Utc::now();
            self.store
                .upsert_turn(TurnRecord {
                    id: turn_id.clone(),
                    thread_id: thread_id.clone(),
                    status: TurnStatus::Running,
                    active_lease_id: Some(lease.lease_id.clone()),
                    last_checkpoint_id: None,
                    created_at: now,
                    updated_at: now,
                })
                .await?;
            let result = self
                .run_turn_inner(
                    &thread_id,
                    &turn_id,
                    TurnStart::Fresh(input),
                    &events,
                    cancellation.clone(),
                    &lease,
                )
                .await;
            let status = match &result {
                Ok(()) => TurnStatus::Completed,
                Err(OpsCodexError::Cancelled) => TurnStatus::Cancelled,
                Err(OpsCodexError::NeedsReconciliation(_)) => TurnStatus::NeedsReconciliation,
                Err(_) => TurnStatus::Failed,
            };
            let _ = self
                .store
                .upsert_turn(TurnRecord {
                    id: turn_id.clone(),
                    thread_id: thread_id.clone(),
                    status,
                    active_lease_id: None,
                    last_checkpoint_id: None,
                    created_at: now,
                    updated_at: Utc::now(),
                })
                .await;
            let _ = self
                .store
                .release_lease(&lease.lease_id, lease.fencing_token)
                .await;
            result
        }
        .await;
        tracing::debug!(
            %thread_id,
            %turn_id,
            duration_ms = turn_started.elapsed().as_millis() as u64,
            result = ?result.as_ref().map(|_| ()).map_err(|error| error.to_string()),
            "agent turn finished"
        );
        match &result {
            Err(OpsCodexError::Cancelled) => {
                RuntimeMetrics::inc(&self.metrics.turns_cancelled);
                let _ = self
                    .emit(
                        &thread_id,
                        Some(turn_id),
                        None,
                        RuntimeEvent::TurnCancelled,
                        &events,
                    )
                    .await;
            }
            Err(error) => {
                RuntimeMetrics::inc(&self.metrics.turns_failed);
                let _ = self
                    .emit(
                        &thread_id,
                        Some(turn_id),
                        None,
                        RuntimeEvent::TurnFailed {
                            error: error.to_string(),
                        },
                        &events,
                    )
                    .await;
            }
            Ok(()) => {
                RuntimeMetrics::inc(&self.metrics.turns_completed);
            }
        }
        result
    }

    async fn run_turn_inner(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        start: TurnStart,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
        lease: &Lease,
    ) -> Result<()> {
        let thread = self.store.get_thread(thread_id).await?;
        let workspace_id = thread.workspace_id;
        if !self.workspaces.is_empty() {
            self.workspaces.require(&workspace_id)?;
        }
        let tools = self.tools_for(&workspace_id)?.clone();
        let skill_input = match &start {
            TurnStart::Fresh(input) => input.clone(),
            TurnStart::Resume(_) => TurnInput {
                content: String::new(),
                incident_context: None,
            },
        };
        let instructions = compose_instructions(
            SYSTEM_INSTRUCTIONS,
            self.workspace_skills.get(workspace_id.as_str()),
            &skill_input,
            self.skill_budget_bytes,
        );
        let mut last_event_id: Option<EventId> = None;
        let mut step = 0_u32;
        match start {
            TurnStart::Fresh(input) => {
                self.emit(
                    thread_id,
                    Some(turn_id.clone()),
                    None,
                    RuntimeEvent::UserMessage {
                        content: input.content.clone(),
                        incident_context: input.incident_context.clone(),
                    },
                    events,
                )
                .await?;
                self.emit(
                    thread_id,
                    Some(turn_id.clone()),
                    None,
                    RuntimeEvent::TurnStarted,
                    events,
                )
                .await?;
                self.write_checkpoint(
                    thread_id,
                    turn_id,
                    step,
                    CheckpointPhase::Queued,
                    ResumePolicy::ReplayModel,
                    None,
                    lease,
                )
                .await?;
            }
            TurnStart::Resume(report) => {
                if report.resume_policy == ResumePolicy::Reconcile {
                    return Err(OpsCodexError::NeedsReconciliation(report.user_action));
                }
                if let Some(checkpoint) = &report.checkpoint {
                    step = checkpoint.step;
                }
                if matches!(
                    report.resume_policy,
                    ResumePolicy::RetryObserve | ResumePolicy::WaitApproval
                ) && let Some(operation) = report
                    .checkpoint
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.pending_operation.clone())
                {
                    last_event_id = Some(
                        self.resume_pending_tool(
                            thread_id,
                            turn_id,
                            &workspace_id,
                            &tools,
                            events,
                            cancellation.clone(),
                            lease,
                            operation,
                            report.resume_policy,
                        )
                        .await?,
                    );
                }
            }
        }

        for _ in 0..self.config.max_steps {
            self.maybe_compact(thread_id, turn_id, events).await?;
            self.write_checkpoint(
                thread_id,
                turn_id,
                step,
                CheckpointPhase::ModelRunning,
                ResumePolicy::ReplayModel,
                None,
                lease,
            )
            .await?;
            let request = ModelRequest {
                instructions: instructions.clone(),
                input: self
                    .store
                    .model_context(thread_id, &self.config.context)
                    .await?,
                tools: tools.schemas(),
            };
            let (response, streamed, model_event_id) = self
                .complete_model(
                    thread_id,
                    turn_id,
                    last_event_id.clone(),
                    request,
                    events,
                    cancellation.clone(),
                )
                .await?;
            last_event_id = model_event_id.or(last_event_id);
            let mut called_tool = false;
            for output in response.outputs {
                match output {
                    ModelOutput::Message { content } => {
                        if !streamed {
                            last_event_id = Some(
                                self.emit(
                                    thread_id,
                                    Some(turn_id.clone()),
                                    last_event_id.clone(),
                                    RuntimeEvent::AssistantDelta {
                                        delta: content.clone(),
                                    },
                                    events,
                                )
                                .await?
                                .event_id,
                            );
                        }
                        let diagnosis = finalize_turn_diagnosis(
                            parse_diagnosis(&content),
                            &self.store.events_after(thread_id, 0).await?,
                        );
                        last_event_id = Some(
                            self.emit(
                                thread_id,
                                Some(turn_id.clone()),
                                last_event_id.clone(),
                                RuntimeEvent::AssistantCompleted {
                                    content,
                                    diagnosis: Some(diagnosis),
                                },
                                events,
                            )
                            .await?
                            .event_id,
                        );
                    }
                    ModelOutput::ToolCall {
                        call_id,
                        name,
                        arguments,
                    } => {
                        called_tool = true;
                        let context = TurnEventContext {
                            thread_id,
                            turn_id,
                            workspace_id: &workspace_id,
                            tools: &tools,
                            events,
                            causation_id: last_event_id.clone(),
                            lease,
                            step,
                        };
                        last_event_id = Some(
                            self.execute_tool(
                                &context,
                                call_id,
                                name,
                                arguments,
                                cancellation.clone(),
                                false,
                            )
                            .await?,
                        );
                    }
                }
            }
            if !called_tool {
                self.write_checkpoint(
                    thread_id,
                    turn_id,
                    step,
                    CheckpointPhase::Completed,
                    ResumePolicy::None,
                    None,
                    lease,
                )
                .await?;
                self.emit(
                    thread_id,
                    Some(turn_id.clone()),
                    last_event_id.clone(),
                    RuntimeEvent::TurnCompleted,
                    events,
                )
                .await?;
                return Ok(());
            }
            step = step.saturating_add(1);
        }
        Err(OpsCodexError::MaxStepsExceeded)
    }

    async fn complete_model(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        causation_id: Option<EventId>,
        request: ModelRequest,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<(ModelResponse, bool, Option<EventId>)> {
        let (sink, mut deltas) = mpsc::unbounded_channel();
        let completion = self.model.complete(request, sink, cancellation.clone());
        tokio::pin!(completion);
        let timeout = tokio::time::sleep(self.config.model_timeout);
        tokio::pin!(timeout);
        let mut streamed = false;
        let model_started = Instant::now();
        let mut last_event_id = causation_id.clone();

        let result = loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                _ = &mut timeout => return Err(OpsCodexError::Timeout("model".into())),
                Some(ModelEvent::MessageDelta(delta)) = deltas.recv() => {
                    streamed = true;
                    last_event_id = Some(self.emit(
                        thread_id,
                        Some(turn_id.clone()),
                        last_event_id.clone(),
                        RuntimeEvent::AssistantDelta { delta },
                        events,
                    ).await?.event_id);
                }
                result = &mut completion => {
                    while let Ok(ModelEvent::MessageDelta(delta)) = deltas.try_recv() {
                        streamed = true;
                        last_event_id = Some(self.emit(
                            thread_id,
                            Some(turn_id.clone()),
                            last_event_id.clone(),
                            RuntimeEvent::AssistantDelta { delta },
                            events,
                        ).await?.event_id);
                    }
                    break result.map(|response| (response, streamed, last_event_id.clone()));
                }
            }
        };
        let duration_ms = model_started.elapsed().as_millis() as u64;
        RuntimeMetrics::add(&self.metrics.model_latency_ms_sum, duration_ms);
        match &result {
            Ok((response, _, _)) => {
                RuntimeMetrics::inc(&self.metrics.model_calls);
                tracing::debug!(
                    %thread_id,
                    %turn_id,
                    duration_ms,
                    response_id = ?response.response_id,
                    input_tokens = response.usage.input_tokens,
                    output_tokens = response.usage.output_tokens,
                    total_tokens = response.usage.total_tokens,
                    "model completion finished"
                );
            }
            Err(error) => {
                RuntimeMetrics::inc(&self.metrics.model_errors);
                tracing::debug!(
                    %thread_id,
                    %turn_id,
                    duration_ms,
                    error = %error,
                    "model completion failed"
                );
            }
        }
        result
    }

    async fn execute_tool(
        &self,
        context: &TurnEventContext<'_>,
        call_id: String,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
        already_authorized: bool,
    ) -> Result<EventId> {
        let mut causation = context.causation_id.clone();
        if !already_authorized {
            let proposed = self
                .emit(
                    context.thread_id,
                    Some(context.turn_id.clone()),
                    causation.clone(),
                    RuntimeEvent::ToolProposed {
                        call_id: call_id.clone(),
                        tool: name.clone(),
                        arguments: arguments.clone(),
                    },
                    context.events,
                )
                .await?;
            causation = Some(proposed.event_id.clone());
        }

        let descriptor = match context.tools.descriptor(&name) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                return self
                    .emit_tool_error(context, causation, call_id, name, arguments, error)
                    .await;
            }
        };
        let pending = PendingOperation {
            operation_id: pending_operation_id(context.turn_id, &call_id),
            call_id: call_id.clone(),
            tool: name.clone(),
            arguments: arguments.clone(),
            effect: descriptor.effect.as_str().to_owned(),
            recovery: descriptor.recovery.map(|mode| match mode {
                crate::extensions::RecoveryMode::NoneNeeded => "none_needed".into(),
                crate::extensions::RecoveryMode::Idempotent => "idempotent".into(),
                crate::extensions::RecoveryMode::NeedsReconciliation => {
                    "needs_reconciliation".into()
                }
            }),
        };
        if !already_authorized {
            match self.policy.evaluate_capability(&descriptor) {
                PolicyDecision::Deny => {
                    causation = Some(
                        self.emit(
                            context.thread_id,
                            Some(context.turn_id.clone()),
                            causation,
                            RuntimeEvent::ToolAuthorized {
                                call_id: call_id.clone(),
                                tool: name.clone(),
                                decision: "deny".into(),
                            },
                            context.events,
                        )
                        .await?
                        .event_id,
                    );
                    return self
                        .emit_tool_error(
                            context,
                            causation,
                            call_id,
                            name,
                            arguments,
                            OpsCodexError::Policy("tool is forbidden".into()),
                        )
                        .await;
                }
                PolicyDecision::Ask => {
                    let request_hash = approval_request_hash(
                        &name,
                        &arguments,
                        Some(descriptor.provenance.schema_hash.as_str()),
                    );
                    let (approval_id, decision) = self.policy.broker().request_with_hash(
                        name.clone(),
                        arguments.clone(),
                        descriptor.provenance.schema_hash.clone(),
                    );
                    let expires_at = Utc::now()
                        + chrono::Duration::from_std(self.config.approval_ttl)
                            .unwrap_or(chrono::Duration::seconds(3600));
                    self.store
                        .put_approval(DurableApproval {
                            approval_id: approval_id.clone(),
                            thread_id: Some(context.thread_id.clone()),
                            turn_id: Some(context.turn_id.clone()),
                            tool: name.clone(),
                            arguments: arguments.clone(),
                            request_hash,
                            schema_hash: Some(descriptor.provenance.schema_hash.clone()),
                            status: ApprovalStatus::Pending,
                            expires_at: Some(expires_at),
                        })
                        .await?;
                    self.write_checkpoint(
                        context.thread_id,
                        context.turn_id,
                        context.step,
                        CheckpointPhase::WaitingApproval,
                        ResumePolicy::WaitApproval,
                        Some(pending.clone()),
                        context.lease,
                    )
                    .await?;
                    causation = Some(
                        self.emit(
                            context.thread_id,
                            Some(context.turn_id.clone()),
                            causation,
                            RuntimeEvent::ApprovalRequired {
                                approval_id: approval_id.clone(),
                                tool: name.clone(),
                                arguments: arguments.clone(),
                            },
                            context.events,
                        )
                        .await?
                        .event_id,
                    );
                    let approved = tokio::select! {
                        _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                        _ = tokio::time::sleep(self.config.approval_ttl) => {
                            let mut expired = self.store.get_approval(&approval_id).await?.unwrap_or(DurableApproval {
                                approval_id: approval_id.clone(),
                                thread_id: Some(context.thread_id.clone()),
                                turn_id: Some(context.turn_id.clone()),
                                tool: name.clone(),
                                arguments: arguments.clone(),
                                request_hash: String::new(),
                                schema_hash: Some(descriptor.provenance.schema_hash.clone()),
                                status: ApprovalStatus::Expired,
                                expires_at: Some(expires_at),
                            });
                            expired.status = ApprovalStatus::Expired;
                            let _ = self.store.put_approval(expired).await;
                            false
                        }
                        result = decision => result.map_err(|_| OpsCodexError::Policy("approval channel closed".into()))?,
                    };
                    if let Some(mut durable) = self.store.get_approval(&approval_id).await? {
                        durable.status = if approved {
                            ApprovalStatus::Approved
                        } else if durable.status == ApprovalStatus::Expired {
                            ApprovalStatus::Expired
                        } else {
                            ApprovalStatus::Rejected
                        };
                        let _ = self.store.put_approval(durable).await;
                    }
                    causation = Some(
                        self.emit(
                            context.thread_id,
                            Some(context.turn_id.clone()),
                            causation,
                            RuntimeEvent::ApprovalResolved {
                                approval_id,
                                approved,
                            },
                            context.events,
                        )
                        .await?
                        .event_id,
                    );
                    let decision = if approved { "allow" } else { "deny" };
                    causation = Some(
                        self.emit(
                            context.thread_id,
                            Some(context.turn_id.clone()),
                            causation,
                            RuntimeEvent::ToolAuthorized {
                                call_id: call_id.clone(),
                                tool: name.clone(),
                                decision: decision.into(),
                            },
                            context.events,
                        )
                        .await?
                        .event_id,
                    );
                    if !approved {
                        return self
                            .emit_tool_error(
                                context,
                                causation,
                                call_id,
                                name,
                                arguments,
                                OpsCodexError::Policy("approval rejected".into()),
                            )
                            .await;
                    }
                    let current = match context.tools.descriptor(&name) {
                        Ok(current) => current,
                        Err(error) => {
                            return self
                                .emit_tool_error(
                                    context, causation, call_id, name, arguments, error,
                                )
                                .await;
                        }
                    };
                    if current.provenance.schema_hash != descriptor.provenance.schema_hash {
                        return self
                            .emit_tool_error(
                                context,
                                causation,
                                call_id,
                                name,
                                arguments,
                                OpsCodexError::Policy(
                                    "capability schema changed; approval invalidated".into(),
                                ),
                            )
                            .await;
                    }
                }
                PolicyDecision::Allow => {
                    causation = Some(
                        self.emit(
                            context.thread_id,
                            Some(context.turn_id.clone()),
                            causation,
                            RuntimeEvent::ToolAuthorized {
                                call_id: call_id.clone(),
                                tool: name.clone(),
                                decision: "allow".into(),
                            },
                            context.events,
                        )
                        .await?
                        .event_id,
                    );
                }
            }
        }

        let resume_policy =
            if crate::runtime::tool_is_retryable(descriptor.effect, descriptor.recovery) {
                ResumePolicy::RetryObserve
            } else {
                ResumePolicy::Reconcile
            };
        self.write_checkpoint(
            context.thread_id,
            context.turn_id,
            context.step,
            CheckpointPhase::ToolRunning,
            resume_policy,
            Some(pending),
            context.lease,
        )
        .await?;

        causation = Some(
            self.emit(
                context.thread_id,
                Some(context.turn_id.clone()),
                causation,
                RuntimeEvent::ToolExecutionStarted {
                    call_id: call_id.clone(),
                    tool: name.clone(),
                },
                context.events,
            )
            .await?
            .event_id,
        );

        let started = Instant::now();
        let tool_cancel = cancellation.child_token();
        let execution = context.tools.execute_with_context(
            &name,
            arguments.clone(),
            ToolInvocation {
                cancellation: tool_cancel.clone(),
                workspace_id: context.workspace_id.clone(),
                thread_id: context.thread_id.clone(),
                store: Some(self.store.clone()),
            },
        );
        let result = tokio::select! {
            _ = cancellation.cancelled() => {
                tool_cancel.cancel();
                return Err(OpsCodexError::Cancelled);
            }
            result = tokio::time::timeout(self.config.tool_timeout, execution) => {
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        tool_cancel.cancel();
                        Err(OpsCodexError::Timeout(format!("tool {name}")))
                    }
                }
            }
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        RuntimeMetrics::add(&self.metrics.tool_latency_ms_sum, duration_ms);
        match result {
            Ok(mut output) => {
                RuntimeMetrics::inc(&self.metrics.tool_calls);
                output.evidence.duration_ms = duration_ms;
                tracing::debug!(
                    thread_id = %context.thread_id,
                    turn_id = %context.turn_id,
                    tool = %name,
                    duration_ms,
                    truncated = output.evidence.truncated,
                    success = true,
                    "tool execution finished"
                );
                self.emit_tool_result(context, causation, call_id, name, output, true)
                    .await
            }
            Err(error) => {
                RuntimeMetrics::inc(&self.metrics.tool_errors);
                tracing::debug!(
                    thread_id = %context.thread_id,
                    turn_id = %context.turn_id,
                    tool = %name,
                    duration_ms,
                    success = false,
                    error = %error,
                    "tool execution failed"
                );
                self.emit_tool_error(context, causation, call_id, name, arguments, error)
                    .await
            }
        }
    }

    async fn emit_tool_error(
        &self,
        context: &TurnEventContext<'_>,
        causation_id: Option<EventId>,
        call_id: String,
        name: String,
        arguments: Value,
        error: OpsCodexError,
    ) -> Result<EventId> {
        let class = error.connector_class();
        let output = ToolOutput {
            content: json!({
                "error": error.to_string(),
                "class": class.as_str(),
                "retryable": class.retryable(),
            }),
            evidence: EvidenceMeta::new(name.clone()).with_query(arguments.to_string()),
        };
        self.emit_tool_result(context, causation_id, call_id, name, output, false)
            .await
    }

    async fn emit_tool_result(
        &self,
        context: &TurnEventContext<'_>,
        causation_id: Option<EventId>,
        call_id: String,
        name: String,
        output: ToolOutput,
        success: bool,
    ) -> Result<EventId> {
        let (content, _) = redact_json(&output.content);
        let mut evidence = output.evidence;
        let mut model_content = content.clone();
        if success {
            let bytes = serde_json::to_vec(&content).unwrap_or_default();
            let artifact_ref = if bytes.len() > self.config.inline_artifact_bytes {
                Some(
                    self.artifacts
                        .put_in(context.workspace_id.as_str(), &bytes)
                        .await?,
                )
            } else {
                None
            };
            evidence = finalize_evidence(
                evidence,
                &content,
                artifact_ref,
                &EvidenceIds {
                    workspace_id: context.workspace_id.clone(),
                    thread_id: context.thread_id.clone(),
                    turn_id: context.turn_id.clone(),
                    tool_call_id: call_id.clone(),
                },
            );
            model_content = model_tool_output(true, &evidence, &content);
        }
        let event_id = self
            .emit(
                context.thread_id,
                Some(context.turn_id.clone()),
                causation_id,
                RuntimeEvent::ToolCompleted {
                    call_id,
                    tool: name,
                    output: model_content,
                    evidence,
                    success,
                },
                context.events,
            )
            .await?
            .event_id;
        self.write_checkpoint(
            context.thread_id,
            context.turn_id,
            context.step,
            CheckpointPhase::ToolCompleted,
            ResumePolicy::SkipCompletedTool,
            None,
            context.lease,
        )
        .await?;
        Ok(event_id)
    }

    async fn emit(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        causation_id: Option<EventId>,
        event: RuntimeEvent,
        events: &broadcast::Sender<EventEnvelope>,
    ) -> Result<EventEnvelope> {
        match self
            .store
            .append_event(AppendEvent {
                thread_id: thread_id.clone(),
                turn_id,
                item_id: None,
                causation_id,
                stream_kind: None,
                event,
            })
            .await
        {
            Ok(envelope) => {
                RuntimeMetrics::inc(&self.metrics.store_appends);
                let _ = events.send(envelope.clone());
                Ok(envelope)
            }
            Err(error) => {
                RuntimeMetrics::inc(&self.metrics.store_errors);
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_checkpoint(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        step: u32,
        phase: CheckpointPhase,
        resume_policy: ResumePolicy,
        pending_operation: Option<PendingOperation>,
        lease: &Lease,
    ) -> Result<()> {
        self.store
            .refresh_lease(&lease.lease_id, lease.fencing_token, self.config.lease_ttl)
            .await?;
        let items = self
            .store
            .model_context(thread_id, &self.config.context)
            .await
            .unwrap_or_default();
        self.store
            .put_checkpoint(CheckpointRecord {
                checkpoint_id: uuid::Uuid::now_v7().to_string(),
                turn_id: turn_id.clone(),
                thread_id: thread_id.clone(),
                step,
                phase,
                context_input_hash: Some(context_input_hash(&items)),
                pending_operation,
                last_committed_seq: self.store.last_seq(thread_id).await.unwrap_or(0),
                resume_policy,
                created_at: Utc::now(),
            })
            .await
    }

    async fn maybe_compact(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        events: &broadcast::Sender<EventEnvelope>,
    ) -> Result<()> {
        let history = self.store.events_after(thread_id, 0).await?;
        let full = crate::store::model_items_from_events(&history);
        let bounded = crate::runtime::build_model_context(full.clone(), &self.config.context);
        if bounded.len() >= full.len() {
            return Ok(());
        }
        let drop_count = full.len() - bounded.len();
        let mut counted = 0usize;
        let mut start = None;
        let mut end = None;
        let mut evidence_ids = Vec::new();
        let mut item_ids = Vec::new();
        let mut covered: Vec<crate::runtime::EventEnvelope> = Vec::new();
        for envelope in &history {
            let contributes = matches!(
                envelope.event,
                RuntimeEvent::UserMessage { .. }
                    | RuntimeEvent::AssistantCompleted { .. }
                    | RuntimeEvent::ToolStarted { .. }
                    | RuntimeEvent::ToolProposed { .. }
                    | RuntimeEvent::ToolCompleted { .. }
            );
            if !contributes {
                continue;
            }
            if counted >= drop_count {
                break;
            }
            start.get_or_insert(envelope.seq);
            end = Some(envelope.seq);
            item_ids.push(envelope.event_id.to_string());
            if let RuntimeEvent::ToolCompleted { evidence, .. } = &envelope.event
                && let Some(id) = &evidence.evidence_id
            {
                evidence_ids.push(id.to_string());
            }
            covered.push(envelope.clone());
            counted += 1;
        }
        let (Some(covers_seq_start), Some(covers_seq_end)) = (start, end) else {
            return Ok(());
        };
        if history.iter().any(|envelope| {
            matches!(
                &envelope.event,
                RuntimeEvent::ContextCompacted {
                    covers_seq_start: existing_start,
                    covers_seq_end: existing_end,
                    ..
                } if *existing_start <= covers_seq_start && *existing_end >= covers_seq_end
            )
        }) {
            return Ok(());
        }
        let summary = local_summary(&covered);
        let _ = self
            .emit(
                thread_id,
                Some(turn_id.clone()),
                None,
                RuntimeEvent::ContextCompacted {
                    summary_id: uuid::Uuid::now_v7().to_string(),
                    covers_seq_start,
                    covers_seq_end,
                    source_item_ids: item_ids,
                    source_evidence_ids: evidence_ids,
                    input_hash: context_input_hash(&full),
                    model_provider: Some("local".into()),
                    model: Some("deterministic-suffix".into()),
                    prompt_version: Some("v0.5".into()),
                    summary,
                },
                events,
            )
            .await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn resume_pending_tool(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        workspace_id: &WorkspaceId,
        tools: &ToolRegistry,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
        lease: &Lease,
        operation: PendingOperation,
        policy: ResumePolicy,
    ) -> Result<EventId> {
        if policy == ResumePolicy::WaitApproval {
            let Some(mut pending) = self.store.approval_for_turn(turn_id).await? else {
                return Err(OpsCodexError::Policy(
                    "resume found no durable approval for this turn".into(),
                ));
            };
            if pending
                .expires_at
                .is_some_and(|expires| expires <= Utc::now())
                && pending.status == ApprovalStatus::Pending
            {
                pending.status = ApprovalStatus::Expired;
                self.store.put_approval(pending.clone()).await?;
            }
            match pending.status {
                ApprovalStatus::Approved => {}
                ApprovalStatus::Rejected | ApprovalStatus::Expired => {
                    let context = TurnEventContext {
                        thread_id,
                        turn_id,
                        workspace_id,
                        tools,
                        events,
                        causation_id: None,
                        lease,
                        step: 0,
                    };
                    return self
                        .emit_tool_error(
                            &context,
                            None,
                            operation.call_id,
                            operation.tool,
                            operation.arguments,
                            OpsCodexError::Policy("approval rejected".into()),
                        )
                        .await;
                }
                ApprovalStatus::Pending => {
                    let receiver = self
                        .policy
                        .broker()
                        .restore(crate::policy::PendingApproval {
                            id: pending.approval_id.clone(),
                            tool: pending.tool.clone(),
                            arguments: pending.arguments.clone(),
                            schema_hash: pending.schema_hash.clone(),
                        });
                    let approved = tokio::select! {
                        _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                        result = receiver => result.unwrap_or(false),
                    };
                    let mut durable = pending.clone();
                    durable.status = if approved {
                        ApprovalStatus::Approved
                    } else {
                        ApprovalStatus::Rejected
                    };
                    self.store.put_approval(durable.clone()).await?;
                    let _ = self
                        .emit(
                            thread_id,
                            Some(turn_id.clone()),
                            None,
                            RuntimeEvent::ApprovalResolved {
                                approval_id: durable.approval_id,
                                approved,
                            },
                            events,
                        )
                        .await;
                    if !approved {
                        let context = TurnEventContext {
                            thread_id,
                            turn_id,
                            workspace_id,
                            tools,
                            events,
                            causation_id: None,
                            lease,
                            step: 0,
                        };
                        return self
                            .emit_tool_error(
                                &context,
                                None,
                                operation.call_id,
                                operation.tool,
                                operation.arguments,
                                OpsCodexError::Policy("approval rejected".into()),
                            )
                            .await;
                    }
                }
            }
            let descriptor = tools.descriptor(&operation.tool)?;
            if pending
                .schema_hash
                .as_ref()
                .is_some_and(|hash| hash != &descriptor.provenance.schema_hash)
            {
                return Err(OpsCodexError::Policy(
                    "capability schema changed; approval invalidated".into(),
                ));
            }
            let current_hash = approval_request_hash(
                &operation.tool,
                &operation.arguments,
                pending.schema_hash.as_deref(),
            );
            if current_hash != pending.request_hash {
                return Err(OpsCodexError::Policy(
                    "approval request hash mismatch; refusing to execute".into(),
                ));
            }
        }
        let context = TurnEventContext {
            thread_id,
            turn_id,
            workspace_id,
            tools,
            events,
            causation_id: None,
            lease,
            step: 0,
        };
        self.execute_tool(
            &context,
            operation.call_id,
            operation.tool,
            operation.arguments,
            cancellation,
            true,
        )
        .await
    }
}

fn finalize_turn_diagnosis(
    diagnosis: crate::evidence::Diagnosis,
    events: &[EventEnvelope],
) -> crate::evidence::Diagnosis {
    let evidence: Vec<_> = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted {
                evidence, success, ..
            } if *success => Some(evidence.clone()),
            _ => None,
        })
        .collect();
    let errors = validate_diagnosis(&diagnosis, &evidence);
    apply_citation_limitations(diagnosis, &errors)
}

fn compose_instructions(
    system: &str,
    skills: Option<&SkillCatalog>,
    input: &TurnInput,
    budget_bytes: usize,
) -> String {
    let Some(skills) = skills.filter(|catalog| !catalog.is_empty()) else {
        return system.to_owned();
    };
    let rendered = skills.render(
        input
            .incident_context
            .as_ref()
            .and_then(|context| context.service.as_deref()),
        &input.content,
        budget_bytes,
    );
    format!("{system}\n\n{rendered}")
}

struct TurnEventContext<'a> {
    thread_id: &'a ThreadId,
    turn_id: &'a TurnId,
    workspace_id: &'a WorkspaceId,
    tools: &'a ToolRegistry,
    events: &'a broadcast::Sender<EventEnvelope>,
    causation_id: Option<EventId>,
    lease: &'a Lease,
    step: u32,
}

struct ActiveThreadGuard {
    active_threads: Arc<Mutex<HashSet<ThreadId>>>,
    thread_id: ThreadId,
}

impl ActiveThreadGuard {
    fn enter(active_threads: Arc<Mutex<HashSet<ThreadId>>>, thread_id: ThreadId) -> Result<Self> {
        let inserted = active_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(thread_id.clone());
        if !inserted {
            return Err(OpsCodexError::TurnAlreadyRunning);
        }
        Ok(Self {
            active_threads,
            thread_id,
        })
    }
}

impl Drop for ActiveThreadGuard {
    fn drop(&mut self) {
        self.active_threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.thread_id);
    }
}
