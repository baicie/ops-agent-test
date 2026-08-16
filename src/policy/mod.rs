use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{
    OpsCodexError, Result,
    extensions::{CapabilityDescriptor, CapabilityEffect, CapabilitySource},
    runtime::ApprovalId,
    tools::ToolRisk,
};

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
    pub schema_hash: Option<String>,
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
        self.insert(PendingApproval {
            id: ApprovalId::new(),
            tool: tool.into(),
            arguments,
            schema_hash: None,
        })
    }

    pub fn request_with_hash(
        &self,
        tool: impl Into<String>,
        arguments: Value,
        schema_hash: impl Into<String>,
    ) -> (ApprovalId, oneshot::Receiver<bool>) {
        self.insert(PendingApproval {
            id: ApprovalId::new(),
            tool: tool.into(),
            arguments,
            schema_hash: Some(schema_hash.into()),
        })
    }

    pub fn restore(&self, request: PendingApproval) -> oneshot::Receiver<bool> {
        let (_id, receiver) = self.insert(request);
        receiver
    }

    fn insert(&self, request: PendingApproval) -> (ApprovalId, oneshot::Receiver<bool>) {
        let id = request.id.clone();
        let (sender, receiver) = oneshot::channel();
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

    pub fn evaluate_capability(&self, descriptor: &CapabilityDescriptor) -> PolicyDecision {
        if !descriptor.enabled {
            return PolicyDecision::Deny;
        }
        match descriptor.effect {
            CapabilityEffect::Observe => PolicyDecision::Allow,
            CapabilityEffect::ChangeReversible | CapabilityEffect::ChangeIrreversible => {
                PolicyDecision::Deny
            }
            CapabilityEffect::ExternalSideEffect => {
                if (descriptor.source == CapabilitySource::Builtin && descriptor.name == "exec")
                    || descriptor.trusted_local
                {
                    PolicyDecision::Ask
                } else {
                    PolicyDecision::Deny
                }
            }
        }
    }

    pub fn broker(&self) -> Arc<ApprovalBroker> {
        self.broker.clone()
    }
}
