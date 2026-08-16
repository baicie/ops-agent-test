use std::{path::PathBuf, str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    model::ModelItem,
    runtime::{
        ContextBudget, EventEnvelope, EvidenceId, Item, RuntimeEvent, Thread, ThreadId,
        ThreadStatus, TurnId, WorkspaceId,
    },
    store::{AppendEvent, EventStore},
};

#[derive(Clone)]
pub struct JsonlStore {
    directory: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub workspace_id: WorkspaceId,
    pub status: ThreadStatus,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl JsonlStore {
    pub async fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to create store directory {}: {error}",
                directory.display()
            ))
        })?;
        Ok(Self {
            directory,
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    pub async fn create_thread(
        &self,
        thread_id: ThreadId,
        workspace_id: WorkspaceId,
    ) -> Result<EventEnvelope> {
        workspace_id.validate()?;
        let _guard = self.mutation_lock.lock().await;
        let path = self.thread_path(&thread_id);
        let envelope = EventEnvelope::new(1, thread_id, None, RuntimeEvent::ThreadCreated)
            .with_workspace(workspace_id);
        let line = encode_line(&envelope)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|error| {
                OpsCodexError::Storage(format!(
                    "failed to create thread log {}: {error}",
                    path.display()
                ))
            })?;
        file.write_all(&line).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to write thread log {}: {error}",
                path.display()
            ))
        })?;
        file.flush().await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to flush thread log {}: {error}",
                path.display()
            ))
        })?;
        Ok(envelope)
    }

    pub async fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
    ) -> Result<EventEnvelope> {
        self.append_event(AppendEvent::new(thread_id.clone(), turn_id, event))
            .await
    }

    pub async fn append_event(&self, command: AppendEvent) -> Result<EventEnvelope> {
        let _guard = self.mutation_lock.lock().await;
        let thread_id = &command.thread_id;
        let path = self.thread_path(thread_id);
        let bytes = read_existing(&path, thread_id).await?;
        let parsed = parse_log(&path, thread_id, &bytes)?;
        let seq = match parsed.events.last() {
            Some(envelope) => envelope.seq.checked_add(1).ok_or_else(|| {
                OpsCodexError::Storage(format!("thread {thread_id} sequence is exhausted"))
            })?,
            None => 1,
        };
        let mut envelope = EventEnvelope::with_causation(
            seq,
            command.thread_id.clone(),
            command.turn_id,
            command.item_id,
            command.causation_id,
            command.event,
        );
        if let Some(stream_kind) = command.stream_kind {
            envelope.stream_kind = stream_kind;
        }
        if let Some(first) = parsed.events.first() {
            envelope.workspace_id = first.workspace_id.clone();
        }
        let mut line = Vec::new();
        match parsed.tail {
            LogTail::Clean => {}
            LogTail::ValidWithoutNewline => line.push(b'\n'),
            LogTail::Partial { valid_bytes } => {
                let file = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .await
                    .map_err(|error| storage_io("open", &path, error))?;
                file.set_len(valid_bytes as u64)
                    .await
                    .map_err(|error| storage_io("truncate", &path, error))?;
            }
        }
        line.extend_from_slice(&encode_line(&envelope)?);

        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .map_err(|error| storage_io("open", &path, error))?;
        file.write_all(&line)
            .await
            .map_err(|error| storage_io("append", &path, error))?;
        file.flush()
            .await
            .map_err(|error| storage_io("flush", &path, error))?;
        Ok(envelope)
    }

    pub async fn events_after(
        &self,
        thread_id: &ThreadId,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>> {
        let _guard = self.mutation_lock.lock().await;
        let path = self.thread_path(thread_id);
        let bytes = read_existing(&path, thread_id).await?;
        Ok(parse_log(&path, thread_id, &bytes)?
            .events
            .into_iter()
            .filter(|envelope| envelope.seq > after_seq)
            .collect())
    }

    pub async fn model_history(
        &self,
        thread_id: &ThreadId,
        limit: usize,
    ) -> Result<Vec<ModelItem>> {
        self.model_context(thread_id, &ContextBudget::items_only(limit))
            .await
    }

    pub async fn model_context(
        &self,
        thread_id: &ThreadId,
        budget: &ContextBudget,
    ) -> Result<Vec<ModelItem>> {
        let events = self.events_after(thread_id, 0).await?;
        Ok(crate::runtime::build_model_context(
            model_items_from_events(&events),
            budget,
        ))
    }

    pub async fn last_seq(&self, thread_id: &ThreadId) -> Result<u64> {
        Ok(self
            .events_after(thread_id, 0)
            .await?
            .last()
            .map(|envelope| envelope.seq)
            .unwrap_or(0))
    }

    pub async fn get_evidence(
        &self,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        let events = self.events_after(thread_id, 0).await?;
        for envelope in events {
            if let RuntimeEvent::ToolCompleted {
                evidence, success, ..
            } = envelope.event
            {
                if !success {
                    continue;
                }
                let id = evidence.evidence_id_or_synthesize(thread_id, envelope.seq);
                if &id == evidence_id {
                    let mut evidence = evidence;
                    evidence.evidence_id = Some(id);
                    return Ok(evidence);
                }
            }
        }
        Err(OpsCodexError::NotFound(format!("evidence {evidence_id}")))
    }

    pub async fn get_evidence_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        let thread = self.get_thread_in(workspace_id, thread_id).await?;
        let _ = thread;
        self.get_evidence(thread_id, evidence_id).await
    }

    pub async fn get_thread(&self, thread_id: &ThreadId) -> Result<Thread> {
        let events = self.events_after(thread_id, 0).await?;
        reconstruct_thread(thread_id, &events)
    }

    pub async fn get_thread_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
    ) -> Result<Thread> {
        let thread = self.get_thread(thread_id).await?;
        crate::workspace::deny_cross_workspace(workspace_id, &thread.workspace_id, "thread")?;
        Ok(thread)
    }

    pub async fn thread(&self, thread_id: &ThreadId) -> Result<Thread> {
        self.get_thread(thread_id).await
    }

    pub async fn summarize_thread(&self, thread_id: &ThreadId) -> Result<ThreadSummary> {
        let thread = self.get_thread(thread_id).await?;
        Ok(summary_from_thread(thread))
    }

    pub async fn list_threads(&self) -> Result<Vec<ThreadSummary>> {
        let mut entries = fs::read_dir(&self.directory).await.map_err(|error| {
            OpsCodexError::Storage(format!(
                "failed to list store directory {}: {error}",
                self.directory.display()
            ))
        })?;
        let mut thread_ids = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| OpsCodexError::Storage(format!("failed to list threads: {error}")))?
        {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if let Ok(thread_id) = ThreadId::from_str(stem) {
                thread_ids.push(thread_id);
            }
        }

        let mut summaries = Vec::with_capacity(thread_ids.len());
        for thread_id in thread_ids {
            summaries.push(self.summarize_thread(&thread_id).await?);
        }
        summaries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.to_string().cmp(&left.id.to_string()))
        });
        Ok(summaries)
    }

    fn thread_path(&self, thread_id: &ThreadId) -> PathBuf {
        self.directory.join(format!("{thread_id}.jsonl"))
    }
}

#[async_trait::async_trait]
impl EventStore for JsonlStore {
    async fn create_thread(
        &self,
        thread_id: ThreadId,
        workspace_id: WorkspaceId,
    ) -> Result<EventEnvelope> {
        JsonlStore::create_thread(self, thread_id, workspace_id).await
    }

    async fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
    ) -> Result<EventEnvelope> {
        JsonlStore::append(self, thread_id, turn_id, event).await
    }

    async fn append_event(&self, command: AppendEvent) -> Result<EventEnvelope> {
        JsonlStore::append_event(self, command).await
    }

    async fn events_after(
        &self,
        thread_id: &ThreadId,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>> {
        JsonlStore::events_after(self, thread_id, after_seq).await
    }

    async fn get_thread(&self, thread_id: &ThreadId) -> Result<Thread> {
        JsonlStore::get_thread(self, thread_id).await
    }

    async fn get_thread_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
    ) -> Result<Thread> {
        JsonlStore::get_thread_in(self, workspace_id, thread_id).await
    }

    async fn list_threads(&self) -> Result<Vec<ThreadSummary>> {
        JsonlStore::list_threads(self).await
    }

    async fn last_seq(&self, thread_id: &ThreadId) -> Result<u64> {
        JsonlStore::last_seq(self, thread_id).await
    }

    async fn model_history(&self, thread_id: &ThreadId, limit: usize) -> Result<Vec<ModelItem>> {
        JsonlStore::model_history(self, thread_id, limit).await
    }

    async fn model_context(
        &self,
        thread_id: &ThreadId,
        budget: &ContextBudget,
    ) -> Result<Vec<ModelItem>> {
        JsonlStore::model_context(self, thread_id, budget).await
    }

    async fn get_evidence(
        &self,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        JsonlStore::get_evidence(self, thread_id, evidence_id).await
    }

    async fn get_evidence_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta> {
        JsonlStore::get_evidence_in(self, workspace_id, thread_id, evidence_id).await
    }
}

fn model_items_from_events(events: &[EventEnvelope]) -> Vec<ModelItem> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::UserMessage {
                content,
                incident_context,
            } => {
                let content = match incident_context {
                    Some(context) => format!("{}\n\n{content}", context.prompt_block()),
                    None => content.clone(),
                };
                Some(ModelItem::UserMessage { content })
            }
            RuntimeEvent::AssistantCompleted { content, .. } => Some(ModelItem::AssistantMessage {
                content: content.clone(),
            }),
            RuntimeEvent::ToolStarted {
                call_id,
                tool,
                arguments,
            }
            | RuntimeEvent::ToolProposed {
                call_id,
                tool,
                arguments,
            } => Some(ModelItem::ToolCall {
                call_id: call_id.clone(),
                name: tool.clone(),
                arguments: arguments.clone(),
            }),
            RuntimeEvent::ToolCompleted {
                call_id, output, ..
            } => Some(ModelItem::ToolResult {
                call_id: call_id.clone(),
                output: output.clone(),
            }),
            _ => None,
        })
        .collect()
}

struct ParsedLog {
    events: Vec<EventEnvelope>,
    tail: LogTail,
}

enum LogTail {
    Clean,
    ValidWithoutNewline,
    Partial { valid_bytes: usize },
}

fn parse_log(path: &std::path::Path, thread_id: &ThreadId, bytes: &[u8]) -> Result<ParsedLog> {
    let complete_bytes = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let mut events = Vec::new();
    if complete_bytes > 0 {
        for (index, line) in bytes[..complete_bytes - 1]
            .split(|byte| *byte == b'\n')
            .enumerate()
        {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let envelope = serde_json::from_slice::<EventEnvelope>(line).map_err(|error| {
                OpsCodexError::Storage(format!(
                    "malformed JSONL record at {}:{}: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            validate_envelope(path, thread_id, &events, &envelope)?;
            events.push(envelope);
        }
    }

    let trailing = &bytes[complete_bytes..];
    let tail = if trailing.is_empty() {
        LogTail::Clean
    } else {
        match serde_json::from_slice::<EventEnvelope>(trailing) {
            Ok(envelope) => {
                validate_envelope(path, thread_id, &events, &envelope)?;
                events.push(envelope);
                LogTail::ValidWithoutNewline
            }
            Err(_) => LogTail::Partial {
                valid_bytes: complete_bytes,
            },
        }
    };
    Ok(ParsedLog { events, tail })
}

fn validate_envelope(
    path: &std::path::Path,
    thread_id: &ThreadId,
    previous: &[EventEnvelope],
    envelope: &EventEnvelope,
) -> Result<()> {
    if &envelope.thread_id != thread_id {
        return Err(OpsCodexError::Storage(format!(
            "thread ID mismatch in {} at sequence {}",
            path.display(),
            envelope.seq
        )));
    }
    let expected = match previous.last() {
        Some(event) => event.seq.checked_add(1).ok_or_else(|| {
            OpsCodexError::Storage(format!(
                "sequence overflow while reading {}",
                path.display()
            ))
        })?,
        None => 1,
    };
    if envelope.seq != expected {
        return Err(OpsCodexError::Storage(format!(
            "non-monotonic sequence in {}: expected {expected}, found {}",
            path.display(),
            envelope.seq
        )));
    }
    if let Some(first) = previous.first()
        && first.workspace_id != envelope.workspace_id
    {
        return Err(OpsCodexError::Storage(format!(
            "workspace mismatch in {} at sequence {}",
            path.display(),
            envelope.seq
        )));
    }
    Ok(())
}

fn reconstruct_thread(thread_id: &ThreadId, events: &[EventEnvelope]) -> Result<Thread> {
    let first = events.first().ok_or_else(|| {
        OpsCodexError::Storage(format!("thread {thread_id} has an empty event log"))
    })?;
    if !matches!(first.event, RuntimeEvent::ThreadCreated) {
        return Err(OpsCodexError::Storage(format!(
            "thread {thread_id} does not start with thread_created"
        )));
    }

    let mut items = Vec::new();
    let mut status = ThreadStatus::Idle;
    for envelope in events {
        match &envelope.event {
            RuntimeEvent::ThreadCreated
            | RuntimeEvent::AssistantDelta { .. }
            | RuntimeEvent::ToolAuthorized { .. }
            | RuntimeEvent::ToolExecutionStarted { .. } => {}
            RuntimeEvent::UserMessage { content, .. } => items.push(Item::UserMessage {
                content: content.clone(),
            }),
            RuntimeEvent::AssistantCompleted { content, .. } => {
                items.push(Item::AssistantMessage {
                    content: content.clone(),
                })
            }
            RuntimeEvent::ToolStarted {
                call_id,
                tool,
                arguments,
            }
            | RuntimeEvent::ToolProposed {
                call_id,
                tool,
                arguments,
            } => items.push(Item::ToolCall {
                call_id: call_id.clone(),
                name: tool.clone(),
                arguments: arguments.clone(),
            }),
            RuntimeEvent::ToolCompleted {
                call_id,
                output,
                evidence,
                ..
            } => items.push(Item::ToolResult {
                call_id: call_id.clone(),
                output: output.clone(),
                evidence: evidence.clone(),
            }),
            RuntimeEvent::ApprovalRequired { approval_id, .. } => {
                status = ThreadStatus::WaitingApproval;
                items.push(Item::Approval {
                    id: approval_id.clone(),
                    approved: None,
                });
            }
            RuntimeEvent::ApprovalResolved {
                approval_id,
                approved,
            } => {
                status = ThreadStatus::Running;
                if let Some(Item::Approval {
                    approved: decision, ..
                }) = items
                    .iter_mut()
                    .rev()
                    .find(|item| matches!(item, Item::Approval { id, .. } if id == approval_id))
                {
                    *decision = Some(*approved);
                }
            }
            RuntimeEvent::TurnStarted => status = ThreadStatus::Running,
            RuntimeEvent::TurnCompleted | RuntimeEvent::TurnCancelled => {
                status = ThreadStatus::Idle
            }
            RuntimeEvent::TurnFailed { .. } => status = ThreadStatus::Failed,
        }
    }
    Ok(Thread {
        id: thread_id.clone(),
        workspace_id: first.workspace_id.clone(),
        items,
        status,
        created_at: first.timestamp,
        updated_at: events
            .last()
            .map_or(first.timestamp, |envelope| envelope.timestamp),
    })
}

fn summary_from_thread(thread: Thread) -> ThreadSummary {
    let title = thread.items.iter().find_map(|item| match item {
        Item::UserMessage { content } => Some(content.chars().take(120).collect()),
        _ => None,
    });
    ThreadSummary {
        id: thread.id,
        workspace_id: thread.workspace_id,
        status: thread.status,
        title,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
    }
}

async fn read_existing(path: &std::path::Path, thread_id: &ThreadId) -> Result<Vec<u8>> {
    match fs::read(path).await {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(OpsCodexError::NotFound(format!("thread {thread_id}")))
        }
        Err(error) => Err(storage_io("read", path, error)),
    }
}

fn encode_line(envelope: &EventEnvelope) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(envelope)
        .map_err(|error| OpsCodexError::Storage(format!("failed to encode event: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn storage_io(action: &str, path: &std::path::Path, error: std::io::Error) -> OpsCodexError {
    OpsCodexError::Storage(format!("failed to {action} {}: {error}", path.display()))
}
