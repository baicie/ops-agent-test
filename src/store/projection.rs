use crate::{
    OpsCodexError, Result,
    model::ModelItem,
    runtime::{EventEnvelope, Item, RuntimeEvent, Thread, ThreadId, ThreadStatus},
    store::ThreadSummary,
};

pub fn model_items_from_events(events: &[EventEnvelope]) -> Vec<ModelItem> {
    let covered = compacted_ranges(events);
    events
        .iter()
        .filter_map(|envelope| {
            if is_covered(envelope.seq, &covered)
                && !matches!(envelope.event, RuntimeEvent::ContextCompacted { .. })
            {
                return None;
            }
            match &envelope.event {
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
            RuntimeEvent::ContextCompacted {
                summary,
                covers_seq_start,
                covers_seq_end,
                source_evidence_ids,
                ..
            } => Some(ModelItem::UserMessage {
                content: format!(
                    "[compacted context seq {covers_seq_start}-{covers_seq_end}; evidence: {}]\n{summary}",
                    source_evidence_ids.join(", ")
                ),
            }),
            _ => None,
        }
        })
        .collect()
}

fn compacted_ranges(events: &[EventEnvelope]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ContextCompacted {
                covers_seq_start,
                covers_seq_end,
                ..
            } => Some((*covers_seq_start, *covers_seq_end)),
            _ => None,
        })
        .collect()
}

fn is_covered(seq: u64, ranges: &[(u64, u64)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| seq >= *start && seq <= *end)
}

pub fn reconstruct_thread(thread_id: &ThreadId, events: &[EventEnvelope]) -> Result<Thread> {
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
        apply_event(&mut items, &mut status, envelope);
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
        parent_thread_id: None,
        forked_at_seq: None,
    })
}

pub fn summary_from_thread(thread: Thread) -> ThreadSummary {
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
        parent_thread_id: thread.parent_thread_id,
        forked_at_seq: thread.forked_at_seq,
    }
}

pub fn title_from_event(event: &RuntimeEvent) -> Option<String> {
    match event {
        RuntimeEvent::UserMessage { content, .. } => Some(content.chars().take(120).collect()),
        _ => None,
    }
}

pub fn status_after(status: ThreadStatus, event: &RuntimeEvent) -> ThreadStatus {
    match event {
        RuntimeEvent::ApprovalRequired { .. } => ThreadStatus::WaitingApproval,
        RuntimeEvent::ApprovalResolved { .. } | RuntimeEvent::TurnStarted => ThreadStatus::Running,
        RuntimeEvent::TurnCompleted | RuntimeEvent::TurnCancelled => ThreadStatus::Idle,
        RuntimeEvent::TurnFailed { .. } => ThreadStatus::Failed,
        _ => status,
    }
}

fn apply_event(items: &mut Vec<Item>, status: &mut ThreadStatus, envelope: &EventEnvelope) {
    *status = status_after(*status, &envelope.event);
    match &envelope.event {
        RuntimeEvent::ThreadCreated
        | RuntimeEvent::AssistantDelta { .. }
        | RuntimeEvent::ToolAuthorized { .. }
        | RuntimeEvent::ToolExecutionStarted { .. }
        | RuntimeEvent::TurnStarted
        | RuntimeEvent::TurnCompleted
        | RuntimeEvent::TurnCancelled
        | RuntimeEvent::TurnFailed { .. } => {}
        RuntimeEvent::UserMessage { content, .. } => items.push(Item::UserMessage {
            content: content.clone(),
        }),
        RuntimeEvent::AssistantCompleted { content, .. } => items.push(Item::AssistantMessage {
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
            items.push(Item::Approval {
                id: approval_id.clone(),
                approved: None,
            });
        }
        RuntimeEvent::ApprovalResolved {
            approval_id,
            approved,
        } => {
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
        RuntimeEvent::ContextCompacted {
            summary_id,
            covers_seq_start,
            covers_seq_end,
            summary,
            source_evidence_ids,
            ..
        } => items.push(Item::Summary {
            summary_id: summary_id.clone(),
            covers_seq_start: *covers_seq_start,
            covers_seq_end: *covers_seq_end,
            summary: summary.clone(),
            source_evidence_ids: source_evidence_ids.clone(),
        }),
    }
}
