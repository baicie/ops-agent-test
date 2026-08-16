use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    action::{
        ActionPlan, ActionPolicyInput, ActionRecord, ActionStatus, DEMO_FAULT_TOOL, K8S_SCALE_TOOL,
        TOOL_VERSION, action_request_hash, deny_extension_remediation, evaluate_action,
        execute_demo_fault, execute_k8s_scale, read_k8s_snapshot, schema_hash_for, transition,
    },
    extensions::CapabilityEffect,
    runtime::{
        ActionId, AgentRuntime, ApprovalId, EventEnvelope, PlanId, RuntimeEvent, ThreadId,
        WorkspaceId,
    },
    tools::KubernetesClient,
};

pub struct RemediationRuntime {
    pub enabled: bool,
    pub kill_switch: AtomicBool,
    pub production_safe: bool,
    pub demo_fault_url: String,
    pub http: reqwest::Client,
    pub kube: HashMap<String, Arc<KubernetesClient>>,
    pub approval_ttl: Duration,
    pub operator: String,
    pub mutation_count: AtomicU64,
}

impl RemediationRuntime {
    pub fn new(
        enabled: bool,
        kill_switch: bool,
        production_safe: bool,
        demo_fault_url: String,
        http: reqwest::Client,
        kube: HashMap<String, Arc<KubernetesClient>>,
        approval_ttl: Duration,
    ) -> Self {
        Self {
            enabled,
            kill_switch: AtomicBool::new(kill_switch),
            production_safe,
            demo_fault_url,
            http,
            kube,
            approval_ttl,
            operator: operator_identity(),
            mutation_count: AtomicU64::new(0),
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            kill_switch: AtomicBool::new(false),
            production_safe: false,
            demo_fault_url: "http://127.0.0.1:8080".into(),
            http: reqwest::Client::new(),
            kube: HashMap::new(),
            approval_ttl: Duration::from_secs(1800),
            operator: operator_identity(),
            mutation_count: AtomicU64::new(0),
        }
    }

    pub fn kill_switch_enabled(&self) -> bool {
        self.kill_switch.load(Ordering::SeqCst)
    }

    pub fn set_kill_switch(&self, enabled: bool) {
        self.kill_switch.store(enabled, Ordering::SeqCst);
    }

    pub fn mutations(&self) -> u64 {
        self.mutation_count.load(Ordering::SeqCst)
    }
}

impl AgentRuntime {
    pub fn set_kill_switch(&self, enabled: bool) {
        self.remediation.set_kill_switch(enabled);
    }

    pub fn kill_switch(&self) -> bool {
        self.remediation.kill_switch_enabled()
    }

    pub fn mutation_count(&self) -> u64 {
        self.remediation.mutations()
    }

    pub async fn propose_action_plan(
        &self,
        thread_id: &ThreadId,
        kind: &str,
        parameters: Value,
        claim_ids: Vec<crate::runtime::ClaimId>,
        cancellation: CancellationToken,
    ) -> Result<ActionRecord> {
        let thread = self.store.get_thread(thread_id).await?;
        let workspace = if self.workspaces.is_empty() {
            None
        } else {
            Some(self.workspaces.require(&thread.workspace_id)?)
        };
        let allow_remediation = workspace.is_some_and(|item| item.allow_remediation);
        let max_effect = workspace.and_then(|item| item.max_effect);
        let mut action = build_action(
            thread_id,
            &thread.workspace_id,
            kind,
            parameters,
            self.remediation.approval_ttl,
        )?;
        let decision = evaluate_action(&ActionPolicyInput {
            workspace_id: &thread.workspace_id,
            allow_remediation,
            max_effect,
            tool_id: &action.tool_id,
            effect: CapabilityEffect::parse(&action.effect)?,
            production_safe: self.remediation.production_safe,
            kill_switch: self.remediation.kill_switch_enabled(),
            remediation_enabled: self.remediation.enabled,
        });
        if deny_extension_remediation(&action.tool_id)
            || decision == crate::policy::PolicyDecision::Deny
        {
            action.status = ActionStatus::PolicyDenied;
            self.persist_action(&action, claim_ids).await?;
            self.audit(
                "action.policy_denied",
                Some(thread.workspace_id.as_str()),
                action.review_summary(),
            )
            .await?;
            self.emit_action(thread_id, &action).await?;
            return Err(OpsCodexError::Policy(
                "remediation is denied by policy, kill switch, or workspace settings".into(),
            ));
        }
        action.status = transition(ActionStatus::Proposed, ActionStatus::DryRun)?;
        let dry = self.run_action(&action, true, cancellation.clone()).await?;
        if dry.uncertain {
            action.status = ActionStatus::Invalid;
            action.dry_run_result = Some(dry.message.clone());
            self.persist_action(&action, claim_ids).await?;
            self.emit_action(thread_id, &action).await?;
            return Err(OpsCodexError::Tool(dry.message));
        }
        action.dry_run_result = Some(dry.message.clone());
        action
            .preconditions
            .extend(dry_run_preconditions(kind, &dry.before));
        action.request_hash = action_request_hash(&action);
        action.status = transition(ActionStatus::DryRun, ActionStatus::AwaitingApproval)?;
        action.approval_id = Some(ApprovalId::new());
        self.persist_action(&action, claim_ids).await?;
        self.audit(
            "action.awaiting_approval",
            Some(thread.workspace_id.as_str()),
            action.review_summary(),
        )
        .await?;
        self.emit_action(thread_id, &action).await?;
        Ok(action)
    }

    pub async fn approve_action(
        &self,
        action_id: &ActionId,
        request_hash: &str,
        approved: bool,
    ) -> Result<ActionRecord> {
        let mut action = self
            .store
            .get_action(action_id)
            .await?
            .ok_or_else(|| OpsCodexError::NotFound(format!("action {action_id}")))?;
        self.expire_if_needed(&mut action).await?;
        if action.status != ActionStatus::AwaitingApproval {
            return Err(OpsCodexError::Policy(format!(
                "action {} is {}",
                action.action_id,
                action.status.as_str()
            )));
        }
        if action.consumed_approval {
            return Err(OpsCodexError::Policy(
                "approval was already consumed".into(),
            ));
        }
        let current = action_request_hash(&action);
        if current != action.request_hash || current != request_hash {
            return Err(OpsCodexError::Policy(
                "approval request hash mismatch; refusing to bind approval".into(),
            ));
        }
        action.status = transition(
            ActionStatus::AwaitingApproval,
            if approved {
                ActionStatus::Authorized
            } else {
                ActionStatus::Rejected
            },
        )?;
        if approved {
            action.consumed_approval = true;
        }
        action.updated_at = Utc::now();
        self.store.put_action(action.clone()).await?;
        self.audit(
            if approved {
                "action.authorized"
            } else {
                "action.rejected"
            },
            Some(action.workspace_id.as_str()),
            action.review_summary(),
        )
        .await?;
        self.emit_action(&action.thread_id, &action).await?;
        Ok(action)
    }

    pub async fn execute_action(
        &self,
        action_id: &ActionId,
        cancellation: CancellationToken,
    ) -> Result<ActionRecord> {
        if self.remediation.kill_switch_enabled() || !self.remediation.enabled {
            return Err(OpsCodexError::Policy(
                "kill switch is blocking change operations".into(),
            ));
        }
        let Some(mut action) = self
            .store
            .claim_action_for_execution(action_id, Utc::now())
            .await?
        else {
            let action = self
                .store
                .get_action(action_id)
                .await?
                .ok_or_else(|| OpsCodexError::NotFound(format!("action {action_id}")))?;
            return Err(OpsCodexError::Policy(format!(
                "action {} is {} and cannot be executed",
                action.action_id,
                action.status.as_str()
            )));
        };
        self.emit_action(&action.thread_id, &action).await?;
        if let Err(error) = self
            .check_preconditions(&action, cancellation.clone())
            .await
        {
            action.status = transition(ActionStatus::PreconditionCheck, ActionStatus::Stale)?;
            action.updated_at = Utc::now();
            self.store.put_action(action.clone()).await?;
            self.emit_action(&action.thread_id, &action).await?;
            return Err(error);
        }
        action.status = transition(ActionStatus::PreconditionCheck, ActionStatus::Executing)?;
        action.updated_at = Utc::now();
        self.store.put_action(action.clone()).await?;
        self.emit_action(&action.thread_id, &action).await?;
        let outcome = match self.run_action(&action, false, cancellation).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let message = format!("action runner failed after execution began: {error}");
                action.status =
                    transition(ActionStatus::Executing, ActionStatus::NeedsReconciliation)?;
                action.updated_at = Utc::now();
                self.store.put_action(action.clone()).await?;
                self.audit(
                    "action.needs_reconciliation",
                    Some(action.workspace_id.as_str()),
                    json!({
                        "action_id": action.action_id,
                        "status": action.status.as_str(),
                        "request_hash": action.request_hash,
                        "committed": Value::Null,
                        "message": message,
                    }),
                )
                .await?;
                self.emit_action(&action.thread_id, &action).await?;
                return Err(OpsCodexError::NeedsReconciliation(message));
            }
        };
        if outcome.committed {
            self.remediation
                .mutation_count
                .fetch_add(1, Ordering::SeqCst);
        }
        action.status = if outcome.uncertain {
            transition(ActionStatus::Executing, ActionStatus::NeedsReconciliation)?
        } else {
            transition(ActionStatus::Executing, ActionStatus::Verifying)?
        };
        if action.status == ActionStatus::Verifying {
            action.status = transition(
                ActionStatus::Verifying,
                if outcome.verified {
                    ActionStatus::Succeeded
                } else {
                    ActionStatus::VerificationFailed
                },
            )?;
            if action.status == ActionStatus::VerificationFailed {
                action.status = transition(
                    ActionStatus::VerificationFailed,
                    ActionStatus::RollbackProposed,
                )?;
                action.rollback_spec = Some(rollback_spec(&action));
            }
        }
        action.updated_at = Utc::now();
        self.store.put_action(action.clone()).await?;
        self.audit(
            &format!("action.{}", action.status.as_str()),
            Some(action.workspace_id.as_str()),
            json!({
                "action_id": action.action_id,
                "status": action.status.as_str(),
                "request_hash": action.request_hash,
                "committed": outcome.committed,
                "message": outcome.message,
            }),
        )
        .await?;
        self.emit_action(&action.thread_id, &action).await?;
        if action.status == ActionStatus::NeedsReconciliation {
            return Err(OpsCodexError::NeedsReconciliation(outcome.message));
        }
        Ok(action)
    }

    pub async fn list_thread_actions(&self, thread_id: &ThreadId) -> Result<Vec<ActionRecord>> {
        self.store.list_actions_for_thread(thread_id).await
    }

    pub async fn verify_audit_log(&self) -> Result<()> {
        crate::action::verify_audit_chain(&self.store.list_audit().await?)
    }

    async fn expire_if_needed(&self, action: &mut ActionRecord) -> Result<()> {
        if action.status == ActionStatus::AwaitingApproval && action.expires_at <= Utc::now() {
            action.status = transition(ActionStatus::AwaitingApproval, ActionStatus::Expired)?;
            action.updated_at = Utc::now();
            self.store.put_action(action.clone()).await?;
        }
        Ok(())
    }

    async fn persist_action(
        &self,
        action: &ActionRecord,
        claim_ids: Vec<crate::runtime::ClaimId>,
    ) -> Result<()> {
        let plan = ActionPlan {
            plan_id: action.plan_id.clone(),
            workspace_id: action.workspace_id.clone(),
            thread_id: action.thread_id.clone(),
            diagnosis_claim_ids: claim_ids,
            actions: vec![action.clone()],
            created_at: action.updated_at,
            expires_at: action.expires_at,
        };
        self.store.put_action_plan(plan).await?;
        self.store.put_action(action.clone()).await
    }

    async fn emit_action(
        &self,
        thread_id: &ThreadId,
        action: &ActionRecord,
    ) -> Result<EventEnvelope> {
        self.store
            .append(
                thread_id,
                None,
                RuntimeEvent::ActionUpdated {
                    action_id: action.action_id.clone(),
                    plan_id: action.plan_id.clone(),
                    status: action.status.as_str().to_owned(),
                    tool: action.tool_id.clone(),
                    request_hash: action.request_hash.clone(),
                    review: action.review_summary(),
                },
            )
            .await
    }

    async fn audit(
        &self,
        operation: &str,
        workspace_id: Option<&str>,
        summary: Value,
    ) -> Result<()> {
        self.store
            .append_audit(&self.remediation.operator, workspace_id, operation, summary)
            .await?;
        Ok(())
    }

    async fn run_action(
        &self,
        action: &ActionRecord,
        dry_run: bool,
        cancellation: CancellationToken,
    ) -> Result<crate::action::ExecutionOutcome> {
        match action.tool_id.as_str() {
            DEMO_FAULT_TOOL => {
                execute_demo_fault(
                    &self.remediation.http,
                    &self.remediation.demo_fault_url,
                    dry_run,
                    cancellation,
                )
                .await
            }
            K8S_SCALE_TOOL => {
                let client = self.kube_for(&action.workspace_id)?;
                let kind = required_str(&action.normalized_parameters, "kind")?;
                let namespace = required_str(&action.normalized_parameters, "namespace")?;
                let name = required_str(&action.normalized_parameters, "name")?;
                let replicas = action.normalized_parameters["replicas"]
                    .as_u64()
                    .ok_or_else(|| OpsCodexError::Protocol("replicas required".into()))?
                    as u32;
                let rv = action
                    .preconditions
                    .iter()
                    .find_map(|item| item.strip_prefix("resourceVersion="))
                    .unwrap_or_default();
                let uid = action
                    .preconditions
                    .iter()
                    .find_map(|item| item.strip_prefix("uid="))
                    .unwrap_or_default();
                execute_k8s_scale(
                    client.as_ref(),
                    kind,
                    namespace,
                    name,
                    replicas,
                    rv,
                    uid,
                    &action.operation_id,
                    dry_run,
                    cancellation,
                )
                .await
            }
            other => Err(OpsCodexError::Policy(format!(
                "`{other}` is not a structured remediation action"
            ))),
        }
    }

    async fn check_preconditions(
        &self,
        action: &ActionRecord,
        cancellation: CancellationToken,
    ) -> Result<()> {
        if action.tool_id == K8S_SCALE_TOOL {
            let client = self.kube_for(&action.workspace_id)?;
            let kind = required_str(&action.normalized_parameters, "kind")?;
            let namespace = required_str(&action.normalized_parameters, "namespace")?;
            let name = required_str(&action.normalized_parameters, "name")?;
            let snapshot =
                read_k8s_snapshot(client.as_ref(), kind, namespace, name, cancellation).await?;
            let uid = snapshot
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let rv = snapshot
                .pointer("/metadata/resourceVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let expected_uid = action
                .preconditions
                .iter()
                .find_map(|item| item.strip_prefix("uid="))
                .unwrap_or_default();
            let expected_rv = action
                .preconditions
                .iter()
                .find_map(|item| item.strip_prefix("resourceVersion="))
                .unwrap_or_default();
            if uid != expected_uid || rv != expected_rv {
                return Err(OpsCodexError::Policy(
                    "kubernetes target changed; refusing to execute stale approval".into(),
                ));
            }
        }
        Ok(())
    }

    fn kube_for(&self, workspace_id: &WorkspaceId) -> Result<Arc<KubernetesClient>> {
        self.remediation
            .kube
            .get(workspace_id.as_str())
            .cloned()
            .ok_or_else(|| {
                OpsCodexError::Policy(format!(
                    "workspace {} has no remediation Kubernetes client",
                    workspace_id.as_str()
                ))
            })
    }
}

fn build_action(
    thread_id: &ThreadId,
    workspace_id: &WorkspaceId,
    kind: &str,
    parameters: Value,
    ttl: Duration,
) -> Result<ActionRecord> {
    let now = Utc::now();
    let expires_at =
        now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(1800));
    let (target, params, effect, expected, verification, blast, preconditions) = match kind {
        DEMO_FAULT_TOOL => {
            if workspace_id.as_str() != "local-demo" {
                return Err(OpsCodexError::Policy(
                    "demo_fault_reset is only registered in the local-demo workspace".into(),
                ));
            }
            let service = parameters
                .get("service")
                .and_then(Value::as_str)
                .unwrap_or("order-service");
            if service != "order-service" {
                return Err(OpsCodexError::Policy(
                    "demo_fault_reset target must be order-service".into(),
                ));
            }
            let mode = parameters
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("normal");
            if mode != "normal" {
                return Err(OpsCodexError::Policy(
                    "demo_fault_reset can only set mode=normal".into(),
                ));
            }
            (
                "demo:order-service".to_owned(),
                json!({"service": "order-service", "mode": "normal"}),
                CapabilityEffect::ChangeReversible.as_str().to_owned(),
                "Set demo fault mode to normal".to_owned(),
                "GET /health returns status=ok and mode=normal".to_owned(),
                "single local demo container".to_owned(),
                vec!["loopback-only".into()],
            )
        }
        K8S_SCALE_TOOL => {
            let kind = required_str(&parameters, "kind")?.to_owned();
            if !matches!(kind.as_str(), "Deployment" | "StatefulSet") {
                return Err(OpsCodexError::Policy(
                    "k8s_scale only supports Deployment or StatefulSet".into(),
                ));
            }
            let namespace = required_str(&parameters, "namespace")?.to_owned();
            let name = required_str(&parameters, "name")?.to_owned();
            let replicas = parameters
                .get("replicas")
                .and_then(Value::as_u64)
                .ok_or_else(|| OpsCodexError::Protocol("replicas required".into()))?;
            if !(1..=16).contains(&replicas) {
                return Err(OpsCodexError::Policy(
                    "k8s_scale replicas must be between 1 and 16".into(),
                ));
            }
            (
                format!("k8s:{namespace}/{kind}/{name}"),
                json!({
                    "kind": kind,
                    "namespace": namespace,
                    "name": name,
                    "replicas": replicas
                }),
                CapabilityEffect::ChangeReversible.as_str().to_owned(),
                format!("Scale {kind}/{name} to {replicas}"),
                "desired and available replicas match the approved count".to_owned(),
                format!("one {kind} in {namespace}"),
                Vec::new(),
            )
        }
        other => {
            return Err(OpsCodexError::Protocol(format!(
                "unsupported remediation kind `{other}`"
            )));
        }
    };
    let action_id = ActionId::new();
    let mut action = ActionRecord {
        plan_id: PlanId::new(),
        workspace_id: workspace_id.clone(),
        thread_id: thread_id.clone(),
        tool_id: kind.to_owned(),
        tool_version: TOOL_VERSION.into(),
        schema_hash: schema_hash_for(kind),
        effect,
        normalized_target: target,
        normalized_parameters: params,
        preconditions,
        expected_effect: expected,
        dry_run_result: None,
        verification_spec: verification,
        rollback_spec: None,
        blast_radius: blast,
        operation_id: format!("{action_id}:1"),
        request_hash: String::new(),
        status: ActionStatus::Proposed,
        approval_id: None,
        consumed_approval: false,
        expires_at,
        updated_at: now,
        action_id,
    };
    action.request_hash = action_request_hash(&action);
    Ok(action)
}

fn dry_run_preconditions(kind: &str, before: &Value) -> Vec<String> {
    if kind != K8S_SCALE_TOOL {
        return Vec::new();
    }
    let source = before;
    let uid = source
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let rv = source
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let replicas = source
        .pointer("/spec/replicas")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    vec![
        format!("uid={uid}"),
        format!("resourceVersion={rv}"),
        format!("previousReplicas={replicas}"),
    ]
}

fn rollback_spec(action: &ActionRecord) -> String {
    if action.tool_id == K8S_SCALE_TOOL {
        let previous = action
            .preconditions
            .iter()
            .find_map(|item| item.strip_prefix("previousReplicas="))
            .unwrap_or("unknown");
        format!("propose a new scale action back to {previous} replicas; do not auto-execute")
    } else {
        "re-diagnose and propose a new action; rollback is never automatic".into()
    }
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| OpsCodexError::Protocol(format!("{key} required")))
}

pub fn operator_identity() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local-operator".into())
}
