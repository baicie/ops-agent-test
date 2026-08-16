use crate::{
    extensions::{CapabilityEffect, RecoveryMode},
    runtime::{EventEnvelope, RuntimeEvent, TurnStatus},
    store::{CheckpointPhase, CheckpointRecord, RecoveryReport, ResumePolicy},
};

pub fn tool_is_retryable(effect: CapabilityEffect, recovery: Option<RecoveryMode>) -> bool {
    if matches!(recovery, Some(RecoveryMode::NeedsReconciliation)) {
        return false;
    }
    if matches!(recovery, Some(RecoveryMode::Idempotent)) {
        return true;
    }
    matches!(effect, CapabilityEffect::Observe)
}

pub fn parse_effect(value: &str) -> CapabilityEffect {
    CapabilityEffect::parse(value).unwrap_or(CapabilityEffect::ExternalSideEffect)
}

pub fn parse_recovery(value: Option<&str>) -> Option<RecoveryMode> {
    value.and_then(|item| RecoveryMode::parse(item).ok())
}

pub fn classify_checkpoint(
    checkpoint: &CheckpointRecord,
    events: &[EventEnvelope],
) -> RecoveryReport {
    let skipped_tools = completed_tools(events);
    let (status, resume_policy, risk, user_action) = match checkpoint.phase {
        CheckpointPhase::Completed => (
            TurnStatus::Completed,
            ResumePolicy::None,
            "none".into(),
            "Turn already completed.".into(),
        ),
        CheckpointPhase::Failed => (
            TurnStatus::Failed,
            ResumePolicy::None,
            "none".into(),
            "Turn already failed.".into(),
        ),
        CheckpointPhase::Cancelled => (
            TurnStatus::Cancelled,
            ResumePolicy::None,
            "none".into(),
            "Turn was cancelled.".into(),
        ),
        CheckpointPhase::NeedsReconciliation => (
            TurnStatus::NeedsReconciliation,
            ResumePolicy::Reconcile,
            "external_state_unknown".into(),
            "Inspect the last change operation; OpsCodex will not retry it automatically.".into(),
        ),
        CheckpointPhase::WaitingApproval => (
            TurnStatus::WaitingApproval,
            ResumePolicy::WaitApproval,
            "approval_required".into(),
            "Resume to continue waiting for the original approval, or reject it.".into(),
        ),
        CheckpointPhase::ToolRunning => classify_tool_running(checkpoint, &skipped_tools),
        CheckpointPhase::ToolCompleted => (
            TurnStatus::Interrupted,
            ResumePolicy::SkipCompletedTool,
            "none".into(),
            "Resume from the next model step. Completed tools will not run again.".into(),
        ),
        CheckpointPhase::Queued | CheckpointPhase::ModelRunning | CheckpointPhase::Interrupted => (
            TurnStatus::Interrupted,
            ResumePolicy::ReplayModel,
            "none".into(),
            "Resume to resend the model request from the last normalized context.".into(),
        ),
    };
    RecoveryReport {
        turn_id: checkpoint.turn_id.clone(),
        thread_id: checkpoint.thread_id.clone(),
        status,
        phase: checkpoint.phase,
        resume_policy,
        risk,
        user_action,
        skipped_tools,
        checkpoint: Some(checkpoint.clone()),
    }
}

fn classify_tool_running(
    checkpoint: &CheckpointRecord,
    skipped_tools: &[String],
) -> (TurnStatus, ResumePolicy, String, String) {
    let Some(operation) = &checkpoint.pending_operation else {
        return (
            TurnStatus::Interrupted,
            ResumePolicy::ReplayModel,
            "none".into(),
            "Resume from the last model step. No pending tool was recorded.".into(),
        );
    };
    if skipped_tools
        .iter()
        .any(|call_id| call_id == &operation.call_id)
    {
        return (
            TurnStatus::Interrupted,
            ResumePolicy::SkipCompletedTool,
            "none".into(),
            "Resume from the next model step. The last tool result is already durable.".into(),
        );
    }
    let effect = parse_effect(&operation.effect);
    let recovery = parse_recovery(operation.recovery.as_deref());
    if tool_is_retryable(effect, recovery) {
        (
            TurnStatus::Interrupted,
            ResumePolicy::RetryObserve,
            "retryable_observe".into(),
            format!(
                "Resume may retry observe tool `{}` with operation {}.",
                operation.tool, operation.operation_id
            ),
        )
    } else {
        (
            TurnStatus::NeedsReconciliation,
            ResumePolicy::Reconcile,
            "external_state_unknown".into(),
            format!(
                "Tool `{}` started a change or side-effecting operation whose result is unknown. Do not retry it automatically.",
                operation.tool
            ),
        )
    }
}

pub fn completed_tools(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            RuntimeEvent::ToolCompleted { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

pub fn pending_operation_id(turn_id: impl ToString, call_id: &str) -> String {
    format!("{}:{call_id}", turn_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{ThreadId, TurnId},
        store::PendingOperation,
    };
    use chrono::Utc;
    use serde_json::json;

    fn checkpoint(phase: CheckpointPhase, operation: Option<PendingOperation>) -> CheckpointRecord {
        CheckpointRecord {
            checkpoint_id: "cp-1".into(),
            turn_id: TurnId::new(),
            thread_id: ThreadId::new(),
            step: 1,
            phase,
            context_input_hash: None,
            pending_operation: operation,
            last_committed_seq: 4,
            resume_policy: ResumePolicy::None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn observe_tool_crash_is_retryable() {
        let report = classify_checkpoint(
            &checkpoint(
                CheckpointPhase::ToolRunning,
                Some(PendingOperation {
                    operation_id: "op-1".into(),
                    call_id: "call-1".into(),
                    tool: "promql_query".into(),
                    arguments: json!({}),
                    effect: "observe".into(),
                    recovery: Some("none_needed".into()),
                }),
            ),
            &[],
        );
        assert_eq!(report.status, TurnStatus::Interrupted);
        assert_eq!(report.resume_policy, ResumePolicy::RetryObserve);
    }

    #[test]
    fn change_tool_crash_needs_reconciliation() {
        let report = classify_checkpoint(
            &checkpoint(
                CheckpointPhase::ToolRunning,
                Some(PendingOperation {
                    operation_id: "op-2".into(),
                    call_id: "call-2".into(),
                    tool: "exec".into(),
                    arguments: json!({"command": "reboot"}),
                    effect: "external_side_effect".into(),
                    recovery: Some("none_needed".into()),
                }),
            ),
            &[],
        );
        assert_eq!(report.status, TurnStatus::NeedsReconciliation);
        assert_eq!(report.resume_policy, ResumePolicy::Reconcile);
    }

    #[test]
    fn completed_tool_is_not_retried() {
        let thread_id = ThreadId::new();
        let turn_id = TurnId::new();
        let events = vec![crate::runtime::EventEnvelope::new(
            3,
            thread_id.clone(),
            Some(turn_id.clone()),
            RuntimeEvent::ToolCompleted {
                call_id: "call-1".into(),
                tool: "promql_query".into(),
                output: json!({"ok": true}),
                evidence: crate::evidence::EvidenceMeta::new("promql_query"),
                success: true,
            },
        )];
        let mut record = checkpoint(
            CheckpointPhase::ToolRunning,
            Some(PendingOperation {
                operation_id: "op-1".into(),
                call_id: "call-1".into(),
                tool: "promql_query".into(),
                arguments: json!({}),
                effect: "observe".into(),
                recovery: None,
            }),
        );
        record.thread_id = thread_id;
        record.turn_id = turn_id;
        let report = classify_checkpoint(&record, &events);
        assert_eq!(report.resume_policy, ResumePolicy::SkipCompletedTool);
        assert_eq!(report.status, TurnStatus::Interrupted);
    }

    #[test]
    fn model_and_approval_boundaries_have_unique_resume_policies() {
        let model = classify_checkpoint(&checkpoint(CheckpointPhase::ModelRunning, None), &[]);
        assert_eq!(model.status, TurnStatus::Interrupted);
        assert_eq!(model.resume_policy, ResumePolicy::ReplayModel);
        let waiting = classify_checkpoint(&checkpoint(CheckpointPhase::WaitingApproval, None), &[]);
        assert_eq!(waiting.status, TurnStatus::WaitingApproval);
        assert_eq!(waiting.resume_policy, ResumePolicy::WaitApproval);
        let completed = classify_checkpoint(&checkpoint(CheckpointPhase::ToolCompleted, None), &[]);
        assert_eq!(completed.resume_policy, ResumePolicy::SkipCompletedTool);
    }
}
