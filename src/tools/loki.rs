use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    ConnectorClass, OpsCodexError, Result,
    evidence::{EvidenceMeta, TimeRange},
    tools::{
        Tool, ToolOutput, ToolRisk, connector, read_bounded_body, truncate_output,
        truncate_output_with_marker,
    },
};

pub struct LokiLogQueryTool {
    client: reqwest::Client,
    endpoint: Url,
    tenant_header: String,
    tenant: Option<String>,
    max_range_seconds: u64,
    max_lines: u32,
    max_output_bytes: usize,
}

impl LokiLogQueryTool {
    pub fn new(
        client: reqwest::Client,
        base_url: impl AsRef<str>,
        tenant_header: impl Into<String>,
        tenant: Option<String>,
        max_range_seconds: u64,
        max_lines: u32,
    ) -> Result<Self> {
        let mut endpoint = Url::parse(base_url.as_ref())
            .map_err(|error| OpsCodexError::Tool(format!("invalid Loki URL: {error}")))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(OpsCodexError::Tool(
                "Loki URL must use http or https".into(),
            ));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let path = format!(
            "{}/loki/api/v1/query_range",
            endpoint.path().trim_end_matches('/')
        );
        endpoint.set_path(&path);
        Ok(Self {
            client,
            endpoint,
            tenant_header: tenant_header.into(),
            tenant,
            max_range_seconds: max_range_seconds.max(1),
            max_lines: max_lines.max(1),
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        })
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogQueryArguments {
    query: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    direction: Option<String>,
}

#[async_trait]
impl Tool for LokiLogQueryTool {
    fn name(&self) -> &str {
        "log_query"
    }

    fn description(&self) -> &str {
        "Query Loki logs for a bounded time range. Returns stream labels and truncated lines as Evidence."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "LogQL query"},
                "start": {"type": "string", "format": "date-time"},
                "end": {"type": "string", "format": "date-time"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "direction": {"type": "string", "enum": ["forward", "backward"]}
            },
            "required": ["query", "start", "end"],
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
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        let arguments: LogQueryArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
        })?;
        if arguments.query.trim().is_empty() {
            return Err(OpsCodexError::connector(
                ConnectorClass::InvalidQuery,
                "log_query query cannot be empty",
            ));
        }
        if arguments.end < arguments.start {
            return Err(OpsCodexError::connector(
                ConnectorClass::InvalidQuery,
                "log_query end must be after start",
            ));
        }
        let range = (arguments.end - arguments.start)
            .num_seconds()
            .unsigned_abs();
        if range > self.max_range_seconds {
            return Err(OpsCodexError::connector(
                ConnectorClass::InvalidQuery,
                format!(
                    "log_query range {}s exceeds max {}s",
                    range, self.max_range_seconds
                ),
            ));
        }
        let limit = arguments
            .limit
            .unwrap_or(self.max_lines)
            .min(self.max_lines);
        let direction = arguments.direction.as_deref().unwrap_or("backward");
        if !matches!(direction, "forward" | "backward") {
            return Err(OpsCodexError::connector(
                ConnectorClass::InvalidQuery,
                "direction must be forward or backward",
            ));
        }

        let started = Instant::now();
        let start_ns = arguments.start.timestamp_nanos_opt().unwrap_or(0);
        let end_ns = arguments.end.timestamp_nanos_opt().unwrap_or(0);
        let query = arguments.query.clone();
        let tenant_header = self.tenant_header.clone();
        let tenant = self.tenant.clone();
        let endpoint = self.endpoint.clone();
        let client = self.client.clone();
        let max_output_bytes = self.max_output_bytes;
        let (body, truncated) = connector::retry_readonly(&cancellation, 3, || {
            let client = client.clone();
            let endpoint = endpoint.clone();
            let query = query.clone();
            let tenant_header = tenant_header.clone();
            let tenant = tenant.clone();
            let cancellation = cancellation.clone();
            async move {
                let mut request = client.get(endpoint).query(&[
                    ("query", query.as_str()),
                    ("start", start_ns.to_string().as_str()),
                    ("end", end_ns.to_string().as_str()),
                    ("limit", limit.to_string().as_str()),
                    ("direction", direction),
                ]);
                if let Some(tenant) = tenant {
                    request = request.header(tenant_header, tenant);
                }
                let response = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                    response = request.send() => response.map_err(|error| {
                        OpsCodexError::connector(ConnectorClass::Unavailable, error.to_string())
                    })?,
                };
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(connector::http_status_error("Loki", status, &body));
                }
                read_bounded_body(response, cancellation, max_output_bytes).await
            }
        })
        .await?;
        let (text, text_truncated) = if truncated {
            truncate_output_with_marker(&body, self.max_output_bytes)
        } else {
            truncate_output(&body, self.max_output_bytes)
        };
        let truncated = truncated || text_truncated;
        let parsed = if truncated {
            json!({"truncated": true, "raw": text})
        } else {
            serde_json::from_slice::<Value>(&body).map_err(|error| {
                OpsCodexError::connector(ConnectorClass::MalformedData, error.to_string())
            })?
        };
        let summary = summarize_loki(&parsed);
        Ok(ToolOutput {
            content: parsed,
            evidence: EvidenceMeta::new("loki")
                .with_query(arguments.query)
                .with_duration_ms(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                .with_truncated(truncated)
                .with_summary(summary)
                .with_time_range(TimeRange {
                    start: arguments.start,
                    end: arguments.end,
                }),
        })
    }
}

fn summarize_loki(value: &Value) -> String {
    let streams = value
        .pointer("/data/result")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    format!("Loki returned {streams} log stream(s)")
}
