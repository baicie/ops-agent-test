use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApprovalId, ThreadId, TurnId};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceMeta {
    pub source: String,
    pub query: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    ThreadCreated,
    UserMessage {
        content: String,
    },
    TurnStarted,
    AssistantDelta {
        delta: String,
    },
    AssistantCompleted {
        content: String,
    },
    ToolStarted {
        call_id: String,
        tool: String,
        arguments: Value,
    },
    ToolCompleted {
        call_id: String,
        tool: String,
        output: Value,
        evidence: EvidenceMeta,
        success: bool,
    },
    ApprovalRequired {
        approval_id: ApprovalId,
        tool: String,
        arguments: Value,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        approved: bool,
    },
    TurnCompleted,
    TurnFailed {
        error: String,
    },
    TurnCancelled,
}

impl RuntimeEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::ThreadCreated => "thread_created",
            Self::UserMessage { .. } => "user_message",
            Self::TurnStarted => "turn_started",
            Self::AssistantDelta { .. } => "assistant_delta",
            Self::AssistantCompleted { .. } => "assistant_completed",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolCompleted { .. } => "tool_completed",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::TurnCompleted => "turn_completed",
            Self::TurnFailed { .. } => "turn_failed",
            Self::TurnCancelled => "turn_cancelled",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub seq: u64,
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub event: RuntimeEvent,
}
