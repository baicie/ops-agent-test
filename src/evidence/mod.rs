mod artifact;
mod diagnosis;
mod redact;

pub use artifact::ArtifactStore;
pub use diagnosis::{
    CitationError, Claim, ClaimKind, Confidence, Diagnosis, apply_citation_limitations,
    parse_diagnosis, validate_diagnosis,
};
pub use redact::{redact_json, redact_text};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::runtime::{EvidenceId, ThreadId, TurnId, WorkspaceId};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    #[default]
    Internal,
    Confidential,
    Secret,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceMeta {
    pub source: String,
    pub query: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<EvidenceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_or_operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_content_type")]
    pub content_type: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "is_default_sensitivity")]
    pub sensitivity: Sensitivity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_default_content_type(value: &str) -> bool {
    value.is_empty() || value == "application/json"
}

fn is_default_sensitivity(value: &Sensitivity) -> bool {
    matches!(value, Sensitivity::Internal)
}

impl EvidenceMeta {
    pub fn new(source: impl Into<String>) -> Self {
        let source = source.into();
        Self {
            source_kind: Some(source.clone()),
            source_ref: Some(source.clone()),
            query_or_operation: None,
            observed_at: Some(Utc::now()),
            time_range: None,
            summary: String::new(),
            artifact_ref: None,
            content_type: "application/json".into(),
            byte_size: 0,
            truncated: false,
            sensitivity: Sensitivity::Internal,
            sha256: String::new(),
            evidence_id: None,
            source,
            query: None,
            timestamp: Utc::now(),
            duration_ms: 0,
        }
    }

    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        let query = query.into();
        self.query = Some(query.clone());
        self.query_or_operation = Some(query);
        self
    }

    pub fn with_duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    pub fn with_truncated(mut self, truncated: bool) -> Self {
        self.truncated = truncated;
        self
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn with_time_range(mut self, time_range: TimeRange) -> Self {
        self.time_range = Some(time_range);
        self
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }

    pub fn evidence_id_or_synthesize(&self, thread_id: &ThreadId, seq: u64) -> EvidenceId {
        self.evidence_id
            .clone()
            .unwrap_or_else(|| synthesize_evidence_id(thread_id, seq, &self.source))
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn synthesize_evidence_id(thread_id: &ThreadId, seq: u64, source: &str) -> EvidenceId {
    const NS: uuid::Uuid = uuid::uuid!("b41c9e20-7d3a-5f18-9c6b-2e4f0a1d8c57");
    EvidenceId::from_uuid(uuid::Uuid::new_v5(
        &NS,
        format!("{thread_id}:{seq}:{source}").as_bytes(),
    ))
}

pub struct EvidenceIds {
    pub workspace_id: WorkspaceId,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub tool_call_id: String,
}

pub fn finalize_evidence(
    mut meta: EvidenceMeta,
    content: &Value,
    artifact_ref: Option<String>,
    ids: &EvidenceIds,
) -> EvidenceMeta {
    let canonical =
        serde_json::to_vec(content).unwrap_or_else(|_| content.to_string().into_bytes());
    let (redacted, _redacted_any) = redact_json(content);
    let redacted_bytes = serde_json::to_vec(&redacted).unwrap_or_else(|_| canonical.clone());
    if meta.summary.is_empty() {
        meta.summary = summarize_value(&redacted, 280);
    }
    meta.byte_size = redacted_bytes.len() as u64;
    meta.sha256 = sha256_hex(&redacted_bytes);
    meta.artifact_ref = artifact_ref;
    meta.evidence_id = Some(EvidenceId::new());
    meta.source_kind = Some(
        meta.source_kind
            .clone()
            .unwrap_or_else(|| meta.source.clone()),
    );
    meta.source_ref = Some(
        meta.source_ref
            .clone()
            .unwrap_or_else(|| meta.source.clone()),
    );
    meta.query_or_operation = meta
        .query_or_operation
        .clone()
        .or_else(|| meta.query.clone());
    meta.observed_at = meta.observed_at.or(Some(meta.timestamp));
    let _ = (
        ids.workspace_id.as_str(),
        &ids.thread_id,
        &ids.turn_id,
        &ids.tool_call_id,
    );
    meta
}

pub fn summarize_value(value: &Value, max_chars: usize) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut end = max_chars.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if text.len() <= max_chars {
        text
    } else {
        format!("{}…", &text[..end.saturating_sub(1).max(1).min(text.len())])
    }
}

pub fn model_tool_output(success: bool, evidence: &EvidenceMeta, content: &Value) -> Value {
    if success {
        let mut output = serde_json::json!({
            "success": true,
            "evidence_id": evidence.evidence_id,
            "summary": evidence.summary,
            "truncated": evidence.truncated,
            "sha256": evidence.sha256,
        });
        if evidence.artifact_ref.is_none() {
            output["content"] = content.clone();
        } else {
            output["artifact_ref"] =
                Value::String(evidence.artifact_ref.clone().unwrap_or_default());
        }
        output
    } else {
        content.clone()
    }
}
