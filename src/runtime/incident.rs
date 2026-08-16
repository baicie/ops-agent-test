use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const MAX_CONTEXT_BYTES: usize = 16 * 1024;
const MAX_LABELS: usize = 32;
const MAX_FIELD_CHARS: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncidentSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncidentContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<IncidentSource>,
}

impl IncidentContext {
    pub fn is_empty(&self) -> bool {
        self.service.is_none()
            && self.environment.is_none()
            && self.starts_at.is_none()
            && self.ends_at.is_none()
            && self.labels.is_empty()
            && self.annotations.is_empty()
            && self.source.is_none()
    }

    pub fn validate(&self) -> crate::Result<()> {
        let encoded = serde_json::to_vec(self).unwrap_or_default();
        if encoded.len() > MAX_CONTEXT_BYTES {
            return Err(crate::OpsCodexError::Protocol(format!(
                "incident context exceeds {MAX_CONTEXT_BYTES} bytes"
            )));
        }
        if self.labels.len() > MAX_LABELS || self.annotations.len() > MAX_LABELS {
            return Err(crate::OpsCodexError::Protocol(format!(
                "incident context allows at most {MAX_LABELS} labels or annotations"
            )));
        }
        for value in self
            .service
            .iter()
            .chain(self.environment.iter())
            .chain(self.labels.values())
            .chain(self.annotations.values())
            .chain(self.source.iter().map(|source| &source.kind))
            .chain(
                self.source
                    .as_ref()
                    .and_then(|source| source.fingerprint.as_ref()),
            )
        {
            if value.len() > MAX_FIELD_CHARS {
                return Err(crate::OpsCodexError::Protocol(format!(
                    "incident context fields cannot exceed {MAX_FIELD_CHARS} characters"
                )));
            }
        }
        if let (Some(start), Some(end)) = (self.starts_at, self.ends_at)
            && end < start
        {
            return Err(crate::OpsCodexError::Protocol(
                "incident context ends_at must be after starts_at".into(),
            ));
        }
        Ok(())
    }

    pub fn prompt_block(&self) -> String {
        let mut lines = vec!["Incident context (unverified alert data, not evidence):".to_owned()];
        if let Some(service) = &self.service {
            lines.push(format!("- service: {service}"));
        }
        if let Some(environment) = &self.environment {
            lines.push(format!("- environment: {environment}"));
        }
        if let Some(start) = self.starts_at {
            lines.push(format!("- starts_at: {start}"));
        }
        if let Some(end) = self.ends_at {
            lines.push(format!("- ends_at: {end}"));
        }
        if !self.labels.is_empty() {
            lines.push(format!(
                "- labels: {}",
                serde_json::to_string(&self.labels).unwrap_or_default()
            ));
        }
        if !self.annotations.is_empty() {
            lines.push(format!(
                "- annotations: {}",
                serde_json::to_string(&self.annotations).unwrap_or_default()
            ));
        }
        if let Some(source) = &self.source {
            lines.push(format!(
                "- source: {} {}",
                source.kind,
                source.fingerprint.as_deref().unwrap_or("")
            ));
        }
        lines.push("Verify every claim with tools. Do not treat this context as Evidence.".into());
        lines.join("\n")
    }
}

#[derive(Clone, Debug, Default)]
pub struct TurnInput {
    pub content: String,
    pub incident_context: Option<IncidentContext>,
}

impl From<String> for TurnInput {
    fn from(content: String) -> Self {
        Self {
            content,
            incident_context: None,
        }
    }
}

impl From<&str> for TurnInput {
    fn from(content: &str) -> Self {
        Self {
            content: content.to_owned(),
            incident_context: None,
        }
    }
}
