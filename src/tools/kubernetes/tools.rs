use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    ConnectorClass, OpsCodexError, Result,
    evidence::{EvidenceMeta, TimeRange},
    tools::{Tool, ToolOutput, ToolRisk, truncate_output},
};

use super::KubernetesClient;

pub struct K8sGetTool {
    client: Arc<KubernetesClient>,
    max_output_bytes: usize,
}

pub struct K8sEventsTool {
    client: Arc<KubernetesClient>,
    max_output_bytes: usize,
}

pub struct K8sLogsTool {
    client: Arc<KubernetesClient>,
    max_output_bytes: usize,
}

impl K8sGetTool {
    pub fn new(client: Arc<KubernetesClient>) -> Self {
        Self {
            client,
            max_output_bytes: crate::tools::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

impl K8sEventsTool {
    pub fn new(client: Arc<KubernetesClient>) -> Self {
        Self {
            client,
            max_output_bytes: crate::tools::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

impl K8sLogsTool {
    pub fn new(client: Arc<KubernetesClient>) -> Self {
        Self {
            client,
            max_output_bytes: crate::tools::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

#[derive(Deserialize)]
struct GetArguments {
    kind: String,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    label_selector: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct EventArguments {
    namespace: String,
    #[serde(default)]
    involved_kind: Option<String>,
    #[serde(default)]
    involved_name: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    start: Option<DateTime<Utc>>,
    #[serde(default)]
    end: Option<DateTime<Utc>>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct LogArguments {
    namespace: String,
    pod: String,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    tail_lines: Option<u32>,
    #[serde(default)]
    since_seconds: Option<u64>,
}

#[async_trait]
impl Tool for K8sGetTool {
    fn name(&self) -> &str {
        "k8s_get"
    }

    fn description(&self) -> &str {
        "Read an allowlisted Kubernetes object summary. Never returns Secret data or write verbs."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string"},
                "namespace": {"type": "string"},
                "name": {"type": "string"},
                "label_selector": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            },
            "required": ["kind"],
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let started = Instant::now();
        let arguments: GetArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
        })?;
        let object = self
            .client
            .get_resource(
                &arguments.kind,
                arguments.namespace.as_deref(),
                arguments.name.as_deref(),
                arguments.label_selector.as_deref(),
                arguments.limit.unwrap_or(20),
                cancellation,
            )
            .await?;
        let encoded = serde_json::to_vec(&object).unwrap_or_default();
        let (text, truncated) = truncate_output(&encoded, self.max_output_bytes);
        let content: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
        Ok(ToolOutput {
            content: json!({
                "cluster": self.client.cluster_alias(),
                "kind": arguments.kind,
                "namespace": arguments.namespace,
                "name": arguments.name,
                "truncated": truncated,
                "object": content
            }),
            evidence: EvidenceMeta::new("kubernetes")
                .with_query(format!(
                    "GET {} {} {}",
                    self.client.cluster_alias(),
                    arguments.kind,
                    arguments.name.as_deref().unwrap_or("<list>")
                ))
                .with_summary(format!(
                    "{} {} in {}",
                    arguments.kind,
                    arguments.name.as_deref().unwrap_or("list"),
                    self.client.cluster_alias()
                ))
                .with_duration_ms(started.elapsed().as_millis() as u64)
                .with_truncated(truncated),
        })
    }
}

#[async_trait]
impl Tool for K8sEventsTool {
    fn name(&self) -> &str {
        "k8s_events"
    }

    fn description(&self) -> &str {
        "Read Kubernetes Events for an allowlisted namespace and involved object."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {"type": "string"},
                "involved_kind": {"type": "string"},
                "involved_name": {"type": "string"},
                "reason": {"type": "string"},
                "start": {"type": "string", "format": "date-time"},
                "end": {"type": "string", "format": "date-time"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            },
            "required": ["namespace"],
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let started = Instant::now();
        let arguments: EventArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
        })?;
        let object = self
            .client
            .list_events(
                &arguments.namespace,
                arguments.involved_kind.as_deref(),
                arguments.involved_name.as_deref(),
                arguments.reason.as_deref(),
                arguments.limit.unwrap_or(20),
                cancellation,
            )
            .await?;
        let mut events = object
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let (Some(start), Some(end)) = (arguments.start, arguments.end) {
            events.retain(|event| event_in_window(event, start, end));
        }
        let content = json!({
            "cluster": self.client.cluster_alias(),
            "namespace": arguments.namespace,
            "count": events.len(),
            "items": events
        });
        Ok(ToolOutput {
            content: content.clone(),
            evidence: EvidenceMeta::new("kubernetes")
                .with_query(format!(
                    "EVENTS {}/{}",
                    self.client.cluster_alias(),
                    arguments.namespace
                ))
                .with_summary(format!(
                    "{} Kubernetes events in {}",
                    events.len(),
                    arguments.namespace
                ))
                .with_time_range(TimeRange {
                    start: arguments.start.unwrap_or_else(Utc::now),
                    end: arguments.end.unwrap_or_else(Utc::now),
                })
                .with_duration_ms(started.elapsed().as_millis() as u64),
        })
    }
}

#[async_trait]
impl Tool for K8sLogsTool {
    fn name(&self) -> &str {
        "k8s_logs"
    }

    fn description(&self) -> &str {
        "Read bounded logs from an allowlisted Pod. Does not support exec, attach, or follow."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {"type": "string"},
                "pod": {"type": "string"},
                "container": {"type": "string"},
                "tail_lines": {"type": "integer", "minimum": 1, "maximum": 200},
                "since_seconds": {"type": "integer", "minimum": 1}
            },
            "required": ["namespace", "pod"],
            "additionalProperties": false
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Safe
    }

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput> {
        let started = Instant::now();
        let arguments: LogArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
        })?;
        let logs = self
            .client
            .pod_logs(
                &arguments.namespace,
                &arguments.pod,
                arguments.container.as_deref(),
                arguments.tail_lines.unwrap_or(100),
                arguments.since_seconds,
                cancellation,
            )
            .await?;
        let (text, truncated) = truncate_output(logs.as_bytes(), self.max_output_bytes);
        Ok(ToolOutput {
            content: json!({
                "cluster": self.client.cluster_alias(),
                "namespace": arguments.namespace,
                "pod": arguments.pod,
                "container": arguments.container,
                "truncated": truncated,
                "logs": text
            }),
            evidence: EvidenceMeta::new("kubernetes")
                .with_query(format!(
                    "LOGS {}/{}/{}",
                    self.client.cluster_alias(),
                    arguments.namespace,
                    arguments.pod
                ))
                .with_summary(format!(
                    "logs from {}/{}",
                    arguments.namespace, arguments.pod
                ))
                .with_duration_ms(started.elapsed().as_millis() as u64)
                .with_truncated(truncated),
        })
    }
}

fn event_in_window(event: &Value, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    let stamp = event
        .pointer("/lastTimestamp")
        .or_else(|| event.pointer("/eventTime"))
        .or_else(|| event.pointer("/metadata/creationTimestamp"))
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    match stamp {
        Some(value) => value >= start && value <= end,
        None => true,
    }
}
