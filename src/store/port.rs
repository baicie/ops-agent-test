use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    OpsCodexError, Result,
    evidence::EvidenceMeta,
    model::ModelItem,
    runtime::{
        ApprovalId, ContextBudget, EventEnvelope, EventId, EvidenceId, ItemId, RuntimeEvent,
        StreamKind, Thread, ThreadId, ThreadStatus, TurnId, WorkspaceId,
    },
};

use super::continuity::{CheckpointRecord, DurableApproval, Lease, TurnRecord};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub workspace_id: WorkspaceId,
    pub status: ThreadStatus,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_at_seq: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct AppendEvent {
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
    pub causation_id: Option<EventId>,
    pub stream_kind: Option<StreamKind>,
    pub event: RuntimeEvent,
}

impl AppendEvent {
    pub fn new(thread_id: ThreadId, turn_id: Option<TurnId>, event: RuntimeEvent) -> Self {
        Self {
            thread_id,
            turn_id,
            item_id: None,
            causation_id: None,
            stream_kind: None,
            event,
        }
    }
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn create_thread(
        &self,
        thread_id: ThreadId,
        workspace_id: WorkspaceId,
    ) -> Result<EventEnvelope>;
    async fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: Option<TurnId>,
        event: RuntimeEvent,
    ) -> Result<EventEnvelope>;
    async fn append_event(&self, command: AppendEvent) -> Result<EventEnvelope>;
    async fn events_after(
        &self,
        thread_id: &ThreadId,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>>;
    async fn get_thread(&self, thread_id: &ThreadId) -> Result<Thread>;
    async fn get_thread_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
    ) -> Result<Thread>;
    async fn list_threads(&self) -> Result<Vec<ThreadSummary>>;
    async fn last_seq(&self, thread_id: &ThreadId) -> Result<u64>;
    async fn model_history(&self, thread_id: &ThreadId, limit: usize) -> Result<Vec<ModelItem>>;
    async fn model_context(
        &self,
        thread_id: &ThreadId,
        budget: &ContextBudget,
    ) -> Result<Vec<ModelItem>>;
    async fn get_evidence(
        &self,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta>;
    async fn get_evidence_in(
        &self,
        workspace_id: &WorkspaceId,
        thread_id: &ThreadId,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceMeta>;

    async fn fork_thread(
        &self,
        _thread_id: &ThreadId,
        _at_seq: u64,
        _title: Option<String>,
    ) -> Result<EventEnvelope> {
        Err(OpsCodexError::Protocol(
            "thread fork requires the sqlite store".into(),
        ))
    }

    async fn thread_lineage(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<(Option<ThreadId>, Option<u64>)> {
        Ok((None, None))
    }

    async fn upsert_turn(&self, _record: TurnRecord) -> Result<()> {
        Ok(())
    }

    async fn get_turn(&self, _turn_id: &TurnId) -> Result<Option<TurnRecord>> {
        Ok(None)
    }

    async fn list_open_turns(&self) -> Result<Vec<TurnRecord>> {
        Ok(Vec::new())
    }

    async fn put_checkpoint(&self, _checkpoint: CheckpointRecord) -> Result<()> {
        Ok(())
    }

    async fn last_checkpoint(&self, _turn_id: &TurnId) -> Result<Option<CheckpointRecord>> {
        Ok(None)
    }

    async fn put_approval(&self, _approval: DurableApproval) -> Result<()> {
        Ok(())
    }

    async fn get_approval(&self, _id: &ApprovalId) -> Result<Option<DurableApproval>> {
        Ok(None)
    }

    async fn list_pending_approvals(&self) -> Result<Vec<DurableApproval>> {
        Ok(Vec::new())
    }

    async fn approval_for_turn(&self, _turn_id: &TurnId) -> Result<Option<DurableApproval>> {
        Ok(None)
    }

    async fn acquire_lease(
        &self,
        turn_id: &TurnId,
        thread_id: &ThreadId,
        owner_id: &str,
        ttl: Duration,
    ) -> Result<Lease> {
        Ok(Lease {
            lease_id: uuid::Uuid::now_v7().to_string(),
            turn_id: turn_id.clone(),
            thread_id: thread_id.clone(),
            owner_id: owner_id.to_owned(),
            expires_at: Utc::now()
                + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(30)),
            fencing_token: 1,
        })
    }

    async fn refresh_lease(
        &self,
        _lease_id: &str,
        _fencing_token: i64,
        _ttl: Duration,
    ) -> Result<()> {
        Ok(())
    }

    async fn release_lease(&self, _lease_id: &str, _fencing_token: i64) -> Result<()> {
        Ok(())
    }

    async fn remember_resume(
        &self,
        _key: &str,
        _turn_id: &TurnId,
        _payload: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    async fn force_release_turn_lease(&self, _turn_id: &TurnId) -> Result<()> {
        Ok(())
    }
}
