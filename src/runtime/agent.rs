use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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
    model::{ModelEvent, ModelOutput, ModelProvider, ModelRequest, ModelResponse},
    policy::{PolicyDecision, PolicyEngine},
    runtime::{ContextBudget, EventId, TurnInput},
    store::{AppendEvent, EventStore},
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
        }
    }
}

pub struct AgentRuntime {
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
    workspaces: Arc<WorkspaceCatalog>,
    workspace_tools: Arc<HashMap<String, ToolRegistry>>,
    policy: PolicyEngine,
    store: Arc<dyn EventStore>,
    artifacts: Arc<ArtifactStore>,
    metrics: Arc<RuntimeMetrics>,
    config: RuntimeConfig,
    turn_slots: Arc<Semaphore>,
    active_threads: Arc<Mutex<HashSet<ThreadId>>>,
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
            self.run_turn_inner(&thread_id, &turn_id, input, &events, cancellation.clone())
                .await
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
        input: TurnInput,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let thread = self.store.get_thread(thread_id).await?;
        let workspace_id = thread.workspace_id;
        if !self.workspaces.is_empty() {
            self.workspaces.require(&workspace_id)?;
        }
        let tools = self.tools_for(&workspace_id)?.clone();
        self.emit(
            thread_id,
            Some(turn_id.clone()),
            None,
            RuntimeEvent::UserMessage {
                content: input.content,
                incident_context: input.incident_context,
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

        let mut last_event_id: Option<EventId> = None;
        for _ in 0..self.config.max_steps {
            let request = ModelRequest {
                instructions: SYSTEM_INSTRUCTIONS.into(),
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
                        };
                        last_event_id = Some(
                            self.execute_tool(
                                &context,
                                call_id,
                                name,
                                arguments,
                                cancellation.clone(),
                            )
                            .await?,
                        );
                    }
                }
            }
            if !called_tool {
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
    ) -> Result<EventId> {
        let proposed = self
            .emit(
                context.thread_id,
                Some(context.turn_id.clone()),
                context.causation_id.clone(),
                RuntimeEvent::ToolProposed {
                    call_id: call_id.clone(),
                    tool: name.clone(),
                    arguments: arguments.clone(),
                },
                context.events,
            )
            .await?;
        let mut causation = Some(proposed.event_id.clone());

        let risk = match context.tools.risk(&name) {
            Ok(risk) => risk,
            Err(error) => {
                return self
                    .emit_tool_error(context, causation, call_id, name, arguments, error)
                    .await;
            }
        };
        match self.policy.evaluate(risk) {
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
                let (approval_id, decision) = self
                    .policy
                    .broker()
                    .request(name.clone(), arguments.clone());
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
                    result = decision => result.map_err(|_| OpsCodexError::Policy("approval channel closed".into()))?,
                };
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
        Ok(self
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
            .event_id)
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

struct TurnEventContext<'a> {
    thread_id: &'a ThreadId,
    turn_id: &'a TurnId,
    workspace_id: &'a WorkspaceId,
    tools: &'a ToolRegistry,
    events: &'a broadcast::Sender<EventEnvelope>,
    causation_id: Option<EventId>,
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
