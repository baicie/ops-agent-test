use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

use crate::{
    OpsCodexError, Result,
    model::ModelItem,
    runtime::{EventEnvelope, Item, RuntimeEvent, Thread, ThreadId, ThreadStatus, TurnId},
};

#[derive(Clone)]
pub struct JsonlStore {
    directory: PathBuf,
    mutation_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThreadSummary {
    pub id: ThreadId,
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

    pub async fn create_thread(&self, thread_id: ThreadId) -> Result<EventEnvelope> {
        let _guard = self.mutation_lock.lock().await;
        let path = self.thread_path(&thread_id);
        let envelope = EventEnvelope {
            seq: 1,
            thread_id,
            turn_id: None,
            timestamp: Utc::now(),
            event: RuntimeEvent::ThreadCreated,
        };
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
        let _guard = self.mutation_lock.lock().await;
        let path = self.thread_path(thread_id);
        let bytes = read_existing(&path, thread_id).await?;
        let parsed = parse_log(&path, thread_id, &bytes)?;
        let seq = match parsed.events.last() {
            Some(envelope) => envelope.seq.checked_add(1).ok_or_else(|| {
                OpsCodexError::Storage(format!("thread {thread_id} sequence is exhausted"))
            })?,
            None => 1,
        };
        let envelope = EventEnvelope {
            seq,
            thread_id: thread_id.clone(),
            turn_id,
            timestamp: Utc::now(),
            event,
        };
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
        let events = self.events_after(thread_id, 0).await?;
        let history: Vec<_> = events
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                RuntimeEvent::UserMessage { content } => Some(ModelItem::UserMessage { content }),
                RuntimeEvent::AssistantCompleted { content } => {
                    Some(ModelItem::AssistantMessage { content })
                }
                RuntimeEvent::ToolStarted {
                    call_id,
                    tool,
                    arguments,
                } => Some(ModelItem::ToolCall {
                    call_id,
                    name: tool,
                    arguments,
                }),
                RuntimeEvent::ToolCompleted {
                    call_id, output, ..
                } => Some(ModelItem::ToolResult { call_id, output }),
                _ => None,
            })
            .collect();
        Ok(trim_model_history(history, limit))
    }

    pub async fn get_thread(&self, thread_id: &ThreadId) -> Result<Thread> {
        let events = self.events_after(thread_id, 0).await?;
        reconstruct_thread(thread_id, &events)
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

/// Keep a bounded suffix of model input without emitting an incomplete tool exchange.
/// Responses API requests require every function call output to have its call in context.
fn trim_model_history(history: Vec<ModelItem>, limit: usize) -> Vec<ModelItem> {
    if limit == 0 || history.is_empty() {
        return Vec::new();
    }

    // First remove tool items that do not have a matching counterpart. This also
    // protects the next request after an interrupted or crashed turn.
    let mut paired = vec![false; history.len()];
    let mut pending_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, item) in history.iter().enumerate() {
        match item {
            ModelItem::ToolCall { call_id, .. } => {
                pending_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(index);
            }
            ModelItem::ToolResult { call_id, .. } => {
                let Some(queue) = pending_calls.get_mut(call_id) else {
                    continue;
                };
                let Some(call_index) = queue.pop_front() else {
                    continue;
                };
                paired[call_index] = true;
                paired[index] = true;
            }
            _ => {}
        }
    }

    let mut valid = Vec::with_capacity(history.len());
    for (index, item) in history.into_iter().enumerate() {
        if paired[index]
            || !matches!(
                &item,
                ModelItem::ToolCall { .. } | ModelItem::ToolResult { .. }
            )
        {
            valid.push(item);
        }
    }
    if valid.len() <= limit {
        return valid;
    }

    // The raw suffix can start between a call and its result. Advance past any
    // crossing pair so the hard item limit is retained without an orphan.
    let mut pairs = Vec::new();
    let mut pending_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, item) in valid.iter().enumerate() {
        match item {
            ModelItem::ToolCall { call_id, .. } => {
                pending_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(index);
            }
            ModelItem::ToolResult { call_id, .. } => {
                if let Some(queue) = pending_calls.get_mut(call_id)
                    && let Some(call_index) = queue.pop_front()
                {
                    pairs.push((call_index, index));
                }
            }
            _ => {}
        }
    }

    let mut start = valid.len() - limit;
    loop {
        let adjusted_start = pairs
            .iter()
            .filter_map(|(call_index, result_index)| {
                (*call_index < start && *result_index >= start).then_some(result_index + 1)
            })
            .max()
            .unwrap_or(start);
        if adjusted_start == start {
            break;
        }
        start = adjusted_start;
    }
    valid.into_iter().skip(start).collect()
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
            RuntimeEvent::ThreadCreated | RuntimeEvent::AssistantDelta { .. } => {}
            RuntimeEvent::UserMessage { content } => items.push(Item::UserMessage {
                content: content.clone(),
            }),
            RuntimeEvent::AssistantCompleted { content } => items.push(Item::AssistantMessage {
                content: content.clone(),
            }),
            RuntimeEvent::ToolStarted {
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
