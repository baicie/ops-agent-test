use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{ApprovalId, ThreadId, TurnId, TurnStatus};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointPhase {
    Queued,
    ModelRunning,
    WaitingApproval,
    ToolRunning,
    ToolCompleted,
    Interrupted,
    NeedsReconciliation,
    Completed,
    Failed,
    Cancelled,
}

impl CheckpointPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::ModelRunning => "model_running",
            Self::WaitingApproval => "waiting_approval",
            Self::ToolRunning => "tool_running",
            Self::ToolCompleted => "tool_completed",
            Self::Interrupted => "interrupted",
            Self::NeedsReconciliation => "needs_reconciliation",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "model_running" => Self::ModelRunning,
            "waiting_approval" => Self::WaitingApproval,
            "tool_running" => Self::ToolRunning,
            "tool_completed" => Self::ToolCompleted,
            "interrupted" => Self::Interrupted,
            "needs_reconciliation" => Self::NeedsReconciliation,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Interrupted,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::NeedsReconciliation
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResumePolicy {
    ReplayModel,
    WaitApproval,
    RetryObserve,
    SkipCompletedTool,
    Reconcile,
    #[default]
    None,
}

impl ResumePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplayModel => "replay_model",
            Self::WaitApproval => "wait_approval",
            Self::RetryObserve => "retry_observe",
            Self::SkipCompletedTool => "skip_completed_tool",
            Self::Reconcile => "reconcile",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "replay_model" => Self::ReplayModel,
            "wait_approval" => Self::WaitApproval,
            "retry_observe" => Self::RetryObserve,
            "skip_completed_tool" => Self::SkipCompletedTool,
            "reconcile" => Self::Reconcile,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingOperation {
    pub operation_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub effect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CheckpointRecord {
    pub checkpoint_id: String,
    pub turn_id: TurnId,
    pub thread_id: ThreadId,
    pub step: u32,
    pub phase: CheckpointPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_operation: Option<PendingOperation>,
    pub last_committed_seq: u64,
    pub resume_policy: ResumePolicy,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    pub id: TurnId,
    pub thread_id: ThreadId,
    pub status: TurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkpoint_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DurableApproval {
    pub approval_id: ApprovalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub tool: String,
    pub arguments: Value,
    pub request_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    pub status: ApprovalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Lease {
    pub lease_id: String,
    pub turn_id: TurnId,
    pub thread_id: ThreadId,
    pub owner_id: String,
    pub expires_at: DateTime<Utc>,
    pub fencing_token: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecoveryReport {
    pub turn_id: TurnId,
    pub thread_id: ThreadId,
    pub status: TurnStatus,
    pub phase: CheckpointPhase,
    pub resume_policy: ResumePolicy,
    pub risk: String,
    pub user_action: String,
    pub skipped_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointRecord>,
}

pub fn approval_request_hash(tool: &str, arguments: &Value, schema_hash: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update(serde_json::to_vec(arguments).unwrap_or_default());
    if let Some(schema_hash) = schema_hash {
        hasher.update(schema_hash.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn context_input_hash(items: &[crate::model::ModelItem]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(items).unwrap_or_default());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
