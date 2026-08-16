use std::collections::{HashMap, VecDeque};

use crate::model::ModelItem;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBudget {
    pub max_items: usize,
    pub max_tokens: usize,
    pub max_bytes: usize,
    pub max_evidence: usize,
    pub max_tool_calls: usize,
    pub max_cost_micros: u64,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_items: 100,
            max_tokens: 24_000,
            max_bytes: 96_000,
            max_evidence: 32,
            max_tool_calls: 24,
            max_cost_micros: 0,
        }
    }
}

impl ContextBudget {
    pub fn items_only(max_items: usize) -> Self {
        Self {
            max_items,
            max_tokens: usize::MAX,
            max_bytes: usize::MAX,
            max_evidence: usize::MAX,
            max_tool_calls: usize::MAX,
            max_cost_micros: 0,
        }
    }
}

pub fn build_model_context(history: Vec<ModelItem>, budget: &ContextBudget) -> Vec<ModelItem> {
    let valid = drop_unpaired(history);
    if valid.is_empty() {
        return valid;
    }
    trim_pair_safe_suffix(valid, |items| {
        items.len() <= budget.max_items
            && count_tool_calls(items) <= budget.max_tool_calls
            && count_evidence(items) <= budget.max_evidence
            && total_bytes(items) <= budget.max_bytes
            && total_tokens(items) <= budget.max_tokens
            && (budget.max_cost_micros == 0
                || estimated_cost_micros(items) <= budget.max_cost_micros)
    })
}

fn drop_unpaired(history: Vec<ModelItem>) -> Vec<ModelItem> {
    let mut paired = vec![false; history.len()];
    let mut pending_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, item) in history.iter().enumerate() {
        match item {
            ModelItem::ToolCall { call_id, .. } => {
                pending_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(index);
            }
            ModelItem::ToolResult { call_id, .. } => {
                let Some(queue) = pending_calls.get_mut(call_id) else {
                    continue;
                };
                let Some(call_index) = queue.pop_front() else {
                    continue;
                };
                paired[call_index] = true;
                paired[index] = true;
            }
            _ => {}
        }
    }

    history
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if paired[index]
                || !matches!(
                    item,
                    ModelItem::ToolCall { .. } | ModelItem::ToolResult { .. }
                )
            {
                Some(item)
            } else {
                None
            }
        })
        .collect()
}

fn trim_pair_safe_suffix(
    valid: Vec<ModelItem>,
    fits: impl Fn(&[ModelItem]) -> bool,
) -> Vec<ModelItem> {
    if fits(&valid) {
        return valid;
    }
    let mut pairs = Vec::new();
    let mut pending_calls: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, item) in valid.iter().enumerate() {
        match item {
            ModelItem::ToolCall { call_id, .. } => {
                pending_calls
                    .entry(call_id.clone())
                    .or_default()
                    .push_back(index);
            }
            ModelItem::ToolResult { call_id, .. } => {
                if let Some(queue) = pending_calls.get_mut(call_id)
                    && let Some(call_index) = queue.pop_front()
                {
                    pairs.push((call_index, index));
                }
            }
            _ => {}
        }
    }

    let mut start = 0;
    while start < valid.len() {
        let suffix = &valid[start..];
        if fits(suffix) {
            let adjusted_start = pairs
                .iter()
                .filter_map(|(call_index, result_index)| {
                    (*call_index < start && *result_index >= start).then_some(result_index + 1)
                })
                .max()
                .unwrap_or(start);
            if adjusted_start == start {
                return valid.into_iter().skip(start).collect();
            }
            start = adjusted_start;
            continue;
        }
        start += 1;
    }
    Vec::new()
}

pub fn local_summary(events: &[crate::runtime::EventEnvelope]) -> String {
    use crate::runtime::RuntimeEvent;

    let mut last_user = None;
    let mut constraints = Vec::new();
    let mut evidence_ids = Vec::new();
    let mut failures = Vec::new();
    let mut approvals = Vec::new();
    for envelope in events {
        match &envelope.event {
            RuntimeEvent::UserMessage { content, .. } => {
                last_user = Some(content.clone());
                if !constraints.iter().any(|item| item == content) {
                    constraints.push(content.clone());
                }
            }
            RuntimeEvent::ToolCompleted {
                tool,
                success,
                evidence,
                ..
            } => {
                if let Some(id) = &evidence.evidence_id {
                    evidence_ids.push(id.to_string());
                }
                if !*success {
                    failures.push(tool.clone());
                }
            }
            RuntimeEvent::ApprovalResolved { approved, .. } => {
                approvals.push(if *approved { "approved" } else { "rejected" });
            }
            _ => {}
        }
    }
    let mut lines =
        vec!["Compacted investigation context. Original events are retained.".to_owned()];
    if let Some(user) = last_user {
        lines.push(format!("Unresolved user request: {user}"));
    }
    if !constraints.is_empty() {
        let kept: Vec<_> = constraints.into_iter().take(8).collect();
        lines.push(format!("User constraints: {}", kept.join(" | ")));
    }
    if !evidence_ids.is_empty() {
        lines.push(format!("Key evidence IDs: {}", evidence_ids.join(", ")));
    }
    if !failures.is_empty() {
        lines.push(format!("Tool failures: {}", failures.join(", ")));
    }
    if !approvals.is_empty() {
        lines.push(format!("Approval results: {}", approvals.join(", ")));
    }
    lines.join("\n")
}

fn count_tool_calls(items: &[ModelItem]) -> usize {
    items
        .iter()
        .filter(|item| matches!(item, ModelItem::ToolCall { .. }))
        .count()
}

fn count_evidence(items: &[ModelItem]) -> usize {
    items
        .iter()
        .filter(|item| matches!(item, ModelItem::ToolResult { .. }))
        .count()
}

fn total_bytes(items: &[ModelItem]) -> usize {
    items.iter().map(item_bytes).sum()
}

fn total_tokens(items: &[ModelItem]) -> usize {
    items.iter().map(item_tokens).sum()
}

fn estimated_cost_micros(items: &[ModelItem]) -> u64 {
    (total_tokens(items) as u64).saturating_mul(10)
}

fn item_bytes(item: &ModelItem) -> usize {
    serde_json::to_vec(item)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn item_tokens(item: &ModelItem) -> usize {
    item_bytes(item).div_ceil(4)
}
