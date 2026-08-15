use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    config::RuntimeSettings,
    model::{ModelEvent, ModelOutput, ModelProvider, ModelRequest, ModelResponse},
    policy::{PolicyDecision, PolicyEngine},
    store::JsonlStore,
    tools::{ToolOutput, ToolRegistry},
};

use super::{EventEnvelope, EvidenceMeta, RuntimeEvent, ThreadId, TurnId};

pub const SYSTEM_INSTRUCTIONS: &str = r#"You are OpsCodex, an autonomous AIOps diagnostic agent.

Your job is to investigate runtime incidents using available tools.

Rules:
1. Gather evidence before reaching conclusions.
2. Prefer structured observability tools over exec.
3. Never claim evidence that was not returned by a tool.
4. Correlate metrics, logs and service health when possible.
5. Do not perform destructive or mutating operations.
6. If evidence is insufficient, say what is missing.
7. Keep investigating until you can provide a useful diagnosis.

Final answers should contain Summary, Evidence, Diagnosis, Confidence, and Recommended next actions."#;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub max_steps: usize,
    pub max_concurrent_turns: usize,
    pub model_timeout: Duration,
    pub tool_timeout: Duration,
    pub context_items: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_concurrent_turns: 4,
            model_timeout: Duration::from_secs(120),
            tool_timeout: Duration::from_secs(30),
            context_items: 100,
        }
    }
}

impl From<&RuntimeSettings> for RuntimeConfig {
    fn from(settings: &RuntimeSettings) -> Self {
        Self {
            max_steps: settings.max_steps,
            max_concurrent_turns: settings.max_concurrent_turns,
            model_timeout: Duration::from_secs(settings.model_timeout_seconds),
            tool_timeout: Duration::from_secs(settings.tool_timeout_seconds),
            context_items: settings.context_items,
        }
    }
}

pub struct AgentRuntime {
    model: Arc<dyn ModelProvider>,
    tools: ToolRegistry,
    policy: PolicyEngine,
    store: Arc<JsonlStore>,
    config: RuntimeConfig,
    turn_slots: Arc<Semaphore>,
    active_threads: Arc<Mutex<HashSet<ThreadId>>>,
}

impl AgentRuntime {
    pub fn new(
        model: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        policy: PolicyEngine,
        store: Arc<JsonlStore>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            model,
            tools,
            policy,
            store,
            turn_slots: Arc::new(Semaphore::new(config.max_concurrent_turns)),
            active_threads: Arc::new(Mutex::new(HashSet::new())),
            config,
        }
    }

    pub fn store(&self) -> Arc<JsonlStore> {
        self.store.clone()
    }

    pub fn policy(&self) -> &PolicyEngine {
        &self.policy
    }

    pub async fn run_turn(
        &self,
        thread_id: ThreadId,
        turn_id: TurnId,
        input: String,
        events: broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let turn_started = Instant::now();
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
                let _ = self
                    .emit(
                        &thread_id,
                        Some(turn_id),
                        RuntimeEvent::TurnCancelled,
                        &events,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .emit(
                        &thread_id,
                        Some(turn_id),
                        RuntimeEvent::TurnFailed {
                            error: error.to_string(),
                        },
                        &events,
                    )
                    .await;
            }
            Ok(()) => {}
        }
        result
    }

    async fn run_turn_inner(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        input: String,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        self.emit(
            thread_id,
            Some(turn_id.clone()),
            RuntimeEvent::UserMessage { content: input },
            events,
        )
        .await?;
        self.emit(
            thread_id,
            Some(turn_id.clone()),
            RuntimeEvent::TurnStarted,
            events,
        )
        .await?;

        for _ in 0..self.config.max_steps {
            let request = ModelRequest {
                instructions: SYSTEM_INSTRUCTIONS.into(),
                input: self
                    .store
                    .model_history(thread_id, self.config.context_items)
                    .await?,
                tools: self.tools.schemas(),
            };
            let (response, streamed) = self
                .complete_model(thread_id, turn_id, request, events, cancellation.clone())
                .await?;
            let mut called_tool = false;
            for output in response.outputs {
                match output {
                    ModelOutput::Message { content } => {
                        if !streamed {
                            self.emit(
                                thread_id,
                                Some(turn_id.clone()),
                                RuntimeEvent::AssistantDelta {
                                    delta: content.clone(),
                                },
                                events,
                            )
                            .await?;
                        }
                        self.emit(
                            thread_id,
                            Some(turn_id.clone()),
                            RuntimeEvent::AssistantCompleted { content },
                            events,
                        )
                        .await?;
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
                            events,
                        };
                        self.execute_tool(&context, call_id, name, arguments, cancellation.clone())
                            .await?;
                    }
                }
            }
            if !called_tool {
                self.emit(
                    thread_id,
                    Some(turn_id.clone()),
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
        request: ModelRequest,
        events: &broadcast::Sender<EventEnvelope>,
        cancellation: CancellationToken,
    ) -> Result<(ModelResponse, bool)> {
        let (sink, mut deltas) = mpsc::unbounded_channel();
        let completion = self.model.complete(request, sink, cancellation.clone());
        tokio::pin!(completion);
        let timeout = tokio::time::sleep(self.config.model_timeout);
        tokio::pin!(timeout);
        let mut streamed = false;
        let model_started = Instant::now();

        let result = loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                _ = &mut timeout => return Err(OpsCodexError::Timeout("model".into())),
                Some(ModelEvent::MessageDelta(delta)) = deltas.recv() => {
                    streamed = true;
                    self.emit(
                        thread_id,
                        Some(turn_id.clone()),
                        RuntimeEvent::AssistantDelta { delta },
                        events,
                    ).await?;
                }
                result = &mut completion => {
                    while let Ok(ModelEvent::MessageDelta(delta)) = deltas.try_recv() {
                        streamed = true;
                        self.emit(
                            thread_id,
                            Some(turn_id.clone()),
                            RuntimeEvent::AssistantDelta { delta },
                            events,
                        ).await?;
                    }
                    break result.map(|response| (response, streamed));
                }
            }
        };
        match &result {
            Ok((response, _)) => tracing::debug!(
                %thread_id,
                %turn_id,
                duration_ms = model_started.elapsed().as_millis() as u64,
                response_id = ?response.response_id,
                input_tokens = response.usage.input_tokens,
                output_tokens = response.usage.output_tokens,
                total_tokens = response.usage.total_tokens,
                "model completion finished"
            ),
            Err(error) => tracing::debug!(
                %thread_id,
                %turn_id,
                duration_ms = model_started.elapsed().as_millis() as u64,
                error = %error,
                "model completion failed"
            ),
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
    ) -> Result<()> {
        self.emit(
            context.thread_id,
            Some(context.turn_id.clone()),
            RuntimeEvent::ToolStarted {
                call_id: call_id.clone(),
                tool: name.clone(),
                arguments: arguments.clone(),
            },
            context.events,
        )
        .await?;

        let risk = match self.tools.risk(&name) {
            Ok(risk) => risk,
            Err(error) => {
                return self
                    .emit_tool_error(context, call_id, name, arguments, error)
                    .await;
            }
        };
        match self.policy.evaluate(risk) {
            PolicyDecision::Deny => {
                return self
                    .emit_tool_error(
                        context,
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
                self.emit(
                    context.thread_id,
                    Some(context.turn_id.clone()),
                    RuntimeEvent::ApprovalRequired {
                        approval_id: approval_id.clone(),
                        tool: name.clone(),
                        arguments: arguments.clone(),
                    },
                    context.events,
                )
                .await?;
                let approved = tokio::select! {
                    _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                    result = decision => result.map_err(|_| OpsCodexError::Policy("approval channel closed".into()))?,
                };
                self.emit(
                    context.thread_id,
                    Some(context.turn_id.clone()),
                    RuntimeEvent::ApprovalResolved {
                        approval_id,
                        approved,
                    },
                    context.events,
                )
                .await?;
                if !approved {
                    return self
                        .emit_tool_error(
                            context,
                            call_id,
                            name,
                            arguments,
                            OpsCodexError::Policy("approval rejected".into()),
                        )
                        .await;
                }
            }
            PolicyDecision::Allow => {}
        }

        let started = Instant::now();
        let tool_cancel = cancellation.child_token();
        let execution = self
            .tools
            .execute(&name, arguments.clone(), tool_cancel.clone());
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
        match result {
            Ok(mut output) => {
                output.evidence.duration_ms = started.elapsed().as_millis() as u64;
                tracing::debug!(
                    thread_id = %context.thread_id,
                    turn_id = %context.turn_id,
                    tool = %name,
                    duration_ms = output.evidence.duration_ms,
                    truncated = output.evidence.truncated,
                    success = true,
                    "tool execution finished"
                );
                self.emit_tool_result(context, call_id, name, output, true)
                    .await
            }
            Err(error) => {
                tracing::debug!(
                    thread_id = %context.thread_id,
                    turn_id = %context.turn_id,
                    tool = %name,
                    duration_ms = started.elapsed().as_millis() as u64,
                    success = false,
                    error = %error,
                    "tool execution failed"
                );
                self.emit_tool_error(context, call_id, name, arguments, error)
                    .await
            }
        }
    }

    async fn emit_tool_error(
        &self,
        context: &TurnEventContext<'_>,
        call_id: String,
        name: String,
        arguments: Value,
        error: OpsCodexError,
    ) -> Result<()> {
        let output = ToolOutput {
            content: json!({"error": error.to_string()}),
            evidence: EvidenceMeta {
                source: name.clone(),
                query: Some(arguments.to_string()),
                timestamp: chrono::Utc::now(),
                duration_ms: 0,
                truncated: false,
            },
        };
        self.emit_tool_result(context, call_id, name, output, false)
            .await
    }

    async fn emit_tool_result(
        &self,
        context: &TurnEventContext<'_>,
        call_id: String,
        name: String,
        output: ToolOutput,
        success: bool,
    ) -> Result<()> {
        self.emit(
            context.thread_id,
            Some(context.turn_id.clone()),
            RuntimeEvent::ToolCompleted {
                call_id,
                tool: name,
                output: output.content,
                evidence: output.evidence,
                success,
            },
            context.events,
        )
        .await?;
        Ok(())
    }

    async fn emit(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
        events: &broadcast::Sender<EventEnvelope>,
    ) -> Result<EventEnvelope> {
        let envelope = self.store.append(thread_id, turn_id, event).await?;
        let _ = events.send(envelope.clone());
        Ok(envelope)
    }
}

struct TurnEventContext<'a> {
    thread_id: &'a ThreadId,
    turn_id: &'a TurnId,
    events: &'a broadcast::Sender<EventEnvelope>,
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
