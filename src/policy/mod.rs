use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{OpsCodexError, Result, runtime::ApprovalId, tools::ToolRisk};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingApproval {
    pub id: ApprovalId,
    pub tool: String,
    pub arguments: Value,
}

struct PendingEntry {
    request: PendingApproval,
    sender: oneshot::Sender<bool>,
}

#[derive(Default)]
pub struct ApprovalBroker {
    pending: std::sync::Mutex<HashMap<ApprovalId, PendingEntry>>,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(
        &self,
        tool: impl Into<String>,
        arguments: Value,
    ) -> (ApprovalId, oneshot::Receiver<bool>) {
        let id = ApprovalId::new();
        let (sender, receiver) = oneshot::channel();
        let request = PendingApproval {
            id: id.clone(),
            tool: tool.into(),
            arguments,
        };
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), PendingEntry { request, sender });
        (id, receiver)
    }

    pub fn resolve(&self, id: &ApprovalId, approved: bool) -> Result<()> {
        let entry = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id)
            .ok_or_else(|| OpsCodexError::NotFound(format!("approval {id}")))?;
        entry
            .sender
            .send(approved)
            .map_err(|_| OpsCodexError::Policy(format!("approval {id} is no longer waiting")))
    }

    pub fn pending(&self) -> Vec<PendingApproval> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, entry| !entry.sender.is_closed());
        let mut requests: Vec<_> = pending
            .values()
            .map(|entry| entry.request.clone())
            .collect();
        requests.sort_unstable_by_key(|request| request.id.to_string());
        requests
    }
}

#[derive(Clone)]
pub struct PolicyEngine {
    broker: Arc<ApprovalBroker>,
}

impl PolicyEngine {
    pub fn new(broker: Arc<ApprovalBroker>) -> Self {
        Self { broker }
    }

    pub fn decision_for(&self, risk: ToolRisk) -> PolicyDecision {
        match risk {
            ToolRisk::Safe => PolicyDecision::Allow,
            ToolRisk::Ask => PolicyDecision::Ask,
            ToolRisk::Forbidden => PolicyDecision::Deny,
        }
    }

    pub fn evaluate(&self, risk: ToolRisk) -> PolicyDecision {
        self.decision_for(risk)
    }

    pub fn broker(&self) -> Arc<ApprovalBroker> {
        self.broker.clone()
    }
}
