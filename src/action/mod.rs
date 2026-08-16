mod runner;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    OpsCodexError, Result,
    extensions::CapabilityEffect,
    runtime::{ActionId, ApprovalId, ClaimId, PlanId, ThreadId, WorkspaceId},
};

pub use runner::{ExecutionOutcome, execute_demo_fault, execute_k8s_scale, read_k8s_snapshot};

pub const DEMO_FAULT_TOOL: &str = "demo_fault_reset";
pub const K8S_SCALE_TOOL: &str = "k8s_scale";
pub const TOOL_VERSION: &str = "1.0.0";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Proposed,
    PolicyDenied,
    DryRun,
    Invalid,
    AwaitingApproval,
    Expired,
    Rejected,
    Authorized,
    PreconditionCheck,
    Stale,
    Executing,
    Verifying,
    Succeeded,
    VerificationFailed,
    RollbackProposed,
    NeedsReconciliation,
}

impl ActionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::PolicyDenied => "policy_denied",
            Self::DryRun => "dry_run",
            Self::Invalid => "invalid",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Expired => "expired",
            Self::Rejected => "rejected",
            Self::Authorized => "authorized",
            Self::PreconditionCheck => "precondition_check",
            Self::Stale => "stale",
            Self::Executing => "executing",
            Self::Verifying => "verifying",
            Self::Succeeded => "succeeded",
            Self::VerificationFailed => "verification_failed",
            Self::RollbackProposed => "rollback_proposed",
            Self::NeedsReconciliation => "needs_reconciliation",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "policy_denied" => Ok(Self::PolicyDenied),
            "dry_run" => Ok(Self::DryRun),
            "invalid" => Ok(Self::Invalid),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "expired" => Ok(Self::Expired),
            "rejected" => Ok(Self::Rejected),
            "authorized" => Ok(Self::Authorized),
            "precondition_check" => Ok(Self::PreconditionCheck),
            "stale" => Ok(Self::Stale),
            "executing" => Ok(Self::Executing),
            "verifying" => Ok(Self::Verifying),
            "succeeded" => Ok(Self::Succeeded),
            "verification_failed" => Ok(Self::VerificationFailed),
            "rollback_proposed" => Ok(Self::RollbackProposed),
            "needs_reconciliation" => Ok(Self::NeedsReconciliation),
            other => Err(OpsCodexError::Protocol(format!(
                "unknown action status `{other}`"
            ))),
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::PolicyDenied
                | Self::Invalid
                | Self::Expired
                | Self::Rejected
                | Self::Stale
                | Self::Succeeded
                | Self::RollbackProposed
                | Self::NeedsReconciliation
        )
    }

    pub fn can_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Proposed, Self::PolicyDenied | Self::DryRun)
                | (Self::DryRun, Self::AwaitingApproval | Self::Invalid)
                | (
                    Self::AwaitingApproval,
                    Self::Expired | Self::Rejected | Self::Authorized
                )
                | (Self::Authorized, Self::Expired | Self::PreconditionCheck)
                | (
                    Self::PreconditionCheck,
                    Self::Stale | Self::Executing | Self::NeedsReconciliation
                )
                | (Self::Executing, Self::Verifying | Self::NeedsReconciliation)
                | (
                    Self::Verifying,
                    Self::Succeeded | Self::VerificationFailed | Self::NeedsReconciliation
                )
                | (Self::VerificationFailed, Self::RollbackProposed)
        )
    }
}

pub fn transition(from: ActionStatus, to: ActionStatus) -> Result<ActionStatus> {
    if from.can_transition(to) {
        Ok(to)
    } else {
        Err(OpsCodexError::Protocol(format!(
            "illegal action transition {} -> {}",
            from.as_str(),
            to.as_str()
        )))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionPlan {
    pub plan_id: PlanId,
    pub workspace_id: WorkspaceId,
    pub thread_id: ThreadId,
    pub diagnosis_claim_ids: Vec<ClaimId>,
    pub actions: Vec<ActionRecord>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionRecord {
    pub action_id: ActionId,
    pub plan_id: PlanId,
    pub workspace_id: WorkspaceId,
    pub thread_id: ThreadId,
    pub tool_id: String,
    pub tool_version: String,
    pub schema_hash: String,
    pub effect: String,
    pub normalized_target: String,
    pub normalized_parameters: Value,
    pub preconditions: Vec<String>,
    pub expected_effect: String,
    pub dry_run_result: Option<String>,
    pub verification_spec: String,
    pub rollback_spec: Option<String>,
    pub blast_radius: String,
    pub operation_id: String,
    pub request_hash: String,
    pub status: ActionStatus,
    pub approval_id: Option<ApprovalId>,
    pub consumed_approval: bool,
    pub expires_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ActionRecord {
    pub fn review_summary(&self) -> Value {
        serde_json::json!({
            "action_id": self.action_id,
            "tool_id": self.tool_id,
            "tool_version": self.tool_version,
            "effect": self.effect,
            "target": self.normalized_target,
            "parameters": self.normalized_parameters,
            "preconditions": self.preconditions,
            "expected_effect": self.expected_effect,
            "dry_run": self.dry_run_result,
            "verification": self.verification_spec,
            "rollback": self.rollback_spec,
            "blast_radius": self.blast_radius,
            "operation_id": self.operation_id,
            "request_hash": self.request_hash,
            "expires_at": self.expires_at,
            "status": self.status.as_str(),
        })
    }
}

pub fn action_request_hash(action: &ActionRecord) -> String {
    let parameters = serde_json::to_vec(&action.normalized_parameters)
        .expect("serializing a serde_json::Value is infallible");
    let expires_at = action.expires_at.to_rfc3339();
    let mut hasher = canonical_hasher(b"action-request");
    update_hash_field(
        &mut hasher,
        b"workspace_id",
        action.workspace_id.as_str().as_bytes(),
    );
    update_hash_field(&mut hasher, b"tool_id", action.tool_id.as_bytes());
    update_hash_field(&mut hasher, b"tool_version", action.tool_version.as_bytes());
    update_hash_field(&mut hasher, b"schema_hash", action.schema_hash.as_bytes());
    update_hash_field(
        &mut hasher,
        b"normalized_target",
        action.normalized_target.as_bytes(),
    );
    update_hash_field(&mut hasher, b"normalized_parameters", &parameters);
    update_hash_string_list(&mut hasher, b"preconditions", &action.preconditions);
    update_hash_field(
        &mut hasher,
        b"verification_spec",
        action.verification_spec.as_bytes(),
    );
    update_hash_field(&mut hasher, b"effect", action.effect.as_bytes());
    update_hash_field(&mut hasher, b"operation_id", action.operation_id.as_bytes());
    update_hash_field(&mut hasher, b"expires_at", expires_at.as_bytes());
    hex(hasher.finalize().as_slice())
}

pub fn schema_hash_for(tool_id: &str) -> String {
    let schema = match tool_id {
        DEMO_FAULT_TOOL => serde_json::json!({
            "type": "object",
            "required": ["service", "mode"],
            "properties": {
                "service": {"const": "order-service"},
                "mode": {"const": "normal"}
            }
        }),
        K8S_SCALE_TOOL => serde_json::json!({
            "type": "object",
            "required": ["kind", "namespace", "name", "replicas"],
            "properties": {
                "kind": {"enum": ["Deployment", "StatefulSet"]},
                "namespace": {"type": "string"},
                "name": {"type": "string"},
                "replicas": {"type": "integer", "minimum": 1, "maximum": 16}
            }
        }),
        _ => serde_json::json!({"type": "object"}),
    };
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&schema).unwrap_or_default());
    hex(hasher.finalize().as_slice())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub seq: i64,
    pub record_hash: String,
    pub previous_hash: String,
    pub actor: String,
    pub workspace_id: Option<String>,
    pub operation: String,
    pub summary: Value,
    pub created_at: DateTime<Utc>,
}

pub fn audit_record_hash(
    previous_hash: &str,
    actor: &str,
    workspace_id: Option<&str>,
    operation: &str,
    summary: &Value,
    created_at: DateTime<Utc>,
) -> String {
    let summary =
        serde_json::to_vec(summary).expect("serializing a serde_json::Value is infallible");
    let created_at = created_at.to_rfc3339();
    let mut hasher = canonical_hasher(b"audit-record");
    update_hash_field(&mut hasher, b"previous_hash", previous_hash.as_bytes());
    update_hash_field(&mut hasher, b"actor", actor.as_bytes());
    update_optional_hash_field(
        &mut hasher,
        b"workspace_id",
        workspace_id.map(str::as_bytes),
    );
    update_hash_field(&mut hasher, b"operation", operation.as_bytes());
    update_hash_field(&mut hasher, b"summary", &summary);
    update_hash_field(&mut hasher, b"created_at", created_at.as_bytes());
    hex(hasher.finalize().as_slice())
}

fn canonical_hasher(domain: &[u8]) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(b"opscodex/hash-preimage/v1");
    update_length_prefixed(&mut hasher, domain);
    hasher
}

fn update_hash_field(hasher: &mut Sha256, name: &[u8], value: &[u8]) {
    update_length_prefixed(hasher, name);
    update_length_prefixed(hasher, value);
}

fn update_hash_string_list(hasher: &mut Sha256, name: &[u8], values: &[String]) {
    update_length_prefixed(hasher, name);
    let count = u64::try_from(values.len()).expect("a field list length fits into u64");
    hasher.update(count.to_be_bytes());
    for value in values {
        update_length_prefixed(hasher, value.as_bytes());
    }
}

fn update_optional_hash_field(hasher: &mut Sha256, name: &[u8], value: Option<&[u8]>) {
    update_length_prefixed(hasher, name);
    match value {
        Some(value) => {
            hasher.update([1]);
            update_length_prefixed(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("an in-memory field length fits into u64");
    hasher.update(length.to_be_bytes());
    hasher.update(value);
}

pub fn verify_audit_chain(records: &[AuditRecord]) -> Result<()> {
    let mut previous = "genesis".to_owned();
    for record in records {
        if record.previous_hash != previous {
            return Err(OpsCodexError::Storage(
                "audit hash chain previous hash mismatch".into(),
            ));
        }
        let expected = audit_record_hash(
            &record.previous_hash,
            &record.actor,
            record.workspace_id.as_deref(),
            &record.operation,
            &record.summary,
            record.created_at,
        );
        if expected != record.record_hash {
            return Err(OpsCodexError::Storage(
                "audit hash chain record hash mismatch".into(),
            ));
        }
        previous = record.record_hash.clone();
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ActionPolicyInput<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub allow_remediation: bool,
    pub max_effect: Option<CapabilityEffect>,
    pub tool_id: &'a str,
    pub effect: CapabilityEffect,
    pub production_safe: bool,
    pub kill_switch: bool,
    pub remediation_enabled: bool,
}

pub fn evaluate_action(input: &ActionPolicyInput<'_>) -> crate::policy::PolicyDecision {
    if input.kill_switch || !input.remediation_enabled || !input.allow_remediation {
        return crate::policy::PolicyDecision::Deny;
    }
    if deny_extension_remediation(input.tool_id) {
        return crate::policy::PolicyDecision::Deny;
    }
    if input.max_effect == Some(CapabilityEffect::Observe)
        && input.effect != CapabilityEffect::Observe
    {
        return crate::policy::PolicyDecision::Deny;
    }
    match input.effect {
        CapabilityEffect::Observe => crate::policy::PolicyDecision::Allow,
        CapabilityEffect::ChangeReversible => crate::policy::PolicyDecision::Ask,
        CapabilityEffect::ChangeIrreversible | CapabilityEffect::ExternalSideEffect => {
            let _ = input.production_safe;
            crate::policy::PolicyDecision::Deny
        }
    }
}

// Placeholder to keep evaluate_action from referencing a non-existent path.
// MCP/Custom Tool cannot be remediation; callers pass those tool ids explicitly.
mod mcp_guard {
    pub fn is_extension_tool(tool_id: &str) -> bool {
        tool_id.starts_with("mcp/") || tool_id.starts_with("custom/")
    }
}

pub fn deny_extension_remediation(tool_id: &str) -> bool {
    mcp_guard::is_extension_tool(tool_id) || tool_id == "exec"
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_cannot_skip_to_executing() {
        let error = transition(ActionStatus::Proposed, ActionStatus::Executing).unwrap_err();
        assert!(error.to_string().contains("illegal action transition"));
    }

    #[test]
    fn request_hash_changes_when_parameters_change() {
        let mut action = sample_action();
        let original = action_request_hash(&action);
        action.normalized_parameters = serde_json::json!({"service": "other", "mode": "normal"});
        assert_ne!(original, action_request_hash(&action));
    }

    #[test]
    fn request_hash_distinguishes_adjacent_field_boundaries() {
        let mut left = sample_action();
        left.tool_id = "ab".into();
        left.tool_version = "c".into();
        let mut right = left.clone();
        right.tool_id = "a".into();
        right.tool_version = "bc".into();

        assert_ne!(action_request_hash(&left), action_request_hash(&right));
    }

    fn sample_action() -> ActionRecord {
        ActionRecord {
            action_id: ActionId::new(),
            plan_id: PlanId::new(),
            workspace_id: WorkspaceId::new("local-demo"),
            thread_id: ThreadId::new(),
            tool_id: DEMO_FAULT_TOOL.into(),
            tool_version: TOOL_VERSION.into(),
            schema_hash: schema_hash_for(DEMO_FAULT_TOOL),
            effect: CapabilityEffect::ChangeReversible.as_str().into(),
            normalized_target: "demo:order-service".into(),
            normalized_parameters: serde_json::json!({"service": "order-service", "mode": "normal"}),
            preconditions: vec!["mode!=normal".into()],
            expected_effect: "fault mode normal".into(),
            dry_run_result: None,
            verification_spec: "GET /health status=ok".into(),
            rollback_spec: None,
            blast_radius: "local demo order-service only".into(),
            operation_id: "op-1".into(),
            request_hash: String::new(),
            status: ActionStatus::Proposed,
            approval_id: None,
            consumed_approval: false,
            expires_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn change_effects_cannot_be_allow() {
        let workspace = WorkspaceId::new("local-demo");
        let reversible = evaluate_action(&ActionPolicyInput {
            workspace_id: &workspace,
            allow_remediation: true,
            max_effect: None,
            tool_id: DEMO_FAULT_TOOL,
            effect: CapabilityEffect::ChangeReversible,
            production_safe: false,
            kill_switch: false,
            remediation_enabled: true,
        });
        assert_eq!(reversible, crate::policy::PolicyDecision::Ask);
        let irreversible = evaluate_action(&ActionPolicyInput {
            workspace_id: &workspace,
            allow_remediation: true,
            max_effect: None,
            tool_id: K8S_SCALE_TOOL,
            effect: CapabilityEffect::ChangeIrreversible,
            production_safe: true,
            kill_switch: false,
            remediation_enabled: true,
        });
        assert_eq!(irreversible, crate::policy::PolicyDecision::Deny);
        assert_eq!(
            evaluate_action(&ActionPolicyInput {
                workspace_id: &workspace,
                allow_remediation: true,
                max_effect: None,
                tool_id: "exec",
                effect: CapabilityEffect::ChangeReversible,
                production_safe: false,
                kill_switch: false,
                remediation_enabled: true,
            }),
            crate::policy::PolicyDecision::Deny
        );
    }

    #[test]
    fn audit_chain_detects_record_tamper() {
        let created = Utc::now();
        let summary = serde_json::json!({"action_id": "a1"});
        let hash = audit_record_hash(
            "genesis",
            "operator",
            Some("local-demo"),
            "action.awaiting_approval",
            &summary,
            created,
        );
        let records = vec![AuditRecord {
            seq: 1,
            record_hash: hash,
            previous_hash: "genesis".into(),
            actor: "operator".into(),
            workspace_id: Some("local-demo".into()),
            operation: "action.awaiting_approval".into(),
            summary: serde_json::json!({"action_id": "tampered"}),
            created_at: created,
        }];
        assert!(
            verify_audit_chain(&records)
                .unwrap_err()
                .to_string()
                .contains("hash")
        );
    }

    #[test]
    fn audit_hash_distinguishes_adjacent_field_boundaries() {
        let created = Utc::now();
        let summary = serde_json::json!({"action_id": "a1"});

        assert_ne!(
            audit_record_hash("genesis", "ab", Some("c"), "operation", &summary, created),
            audit_record_hash("genesis", "a", Some("bc"), "operation", &summary, created)
        );
    }
}
