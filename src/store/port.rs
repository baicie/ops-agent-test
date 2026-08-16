use async_trait::async_trait;

use crate::{
    Result,
    evidence::EvidenceMeta,
    model::ModelItem,
    runtime::{
        ContextBudget, EventEnvelope, EventId, EvidenceId, ItemId, RuntimeEvent, StreamKind,
        Thread, ThreadId, TurnId, WorkspaceId,
    },
};

use super::jsonl::ThreadSummary;

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
}
