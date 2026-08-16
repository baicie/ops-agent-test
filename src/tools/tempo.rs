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

pub struct TempoTraceSearchTool {
    client: reqwest::Client,
    endpoint: Url,
    max_range_seconds: u64,
    max_output_bytes: usize,
}

pub struct TempoTraceGetTool {
    client: reqwest::Client,
    base: Url,
    max_output_bytes: usize,
}

impl TempoTraceSearchTool {
    pub fn new(
        client: reqwest::Client,
        base_url: impl AsRef<str>,
        max_range_seconds: u64,
    ) -> Result<Self> {
        Ok(Self {
            client,
            endpoint: tempo_url(base_url.as_ref(), "/api/search")?,
            max_range_seconds: max_range_seconds.max(1),
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        })
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

impl TempoTraceGetTool {
    pub fn new(client: reqwest::Client, base_url: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            client,
            base: tempo_url(base_url.as_ref(), "/")?,
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        })
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }
}

fn tempo_url(base: &str, path: &str) -> Result<Url> {
    let mut endpoint = Url::parse(base)
        .map_err(|error| OpsCodexError::Tool(format!("invalid Tempo URL: {error}")))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(OpsCodexError::Tool(
            "Tempo URL must use http or https".into(),
        ));
    }
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let joined = format!("{}{}", endpoint.path().trim_end_matches('/'), path);
    endpoint.set_path(&joined);
    Ok(endpoint)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceSearchArguments {
    #[serde(default)]
    service: Option<String>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    #[serde(default)]
    min_duration: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceGetArguments {
    trace_id: String,
}

#[async_trait]
impl Tool for TempoTraceSearchTool {
    fn name(&self) -> &str {
        "trace_search"
    }

    fn description(&self) -> &str {
        "Search Tempo traces by service and time range. Returns bounded trace summaries as Evidence."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "service": {"type": "string"},
                "start": {"type": "string", "format": "date-time"},
                "end": {"type": "string", "format": "date-time"},
                "min_duration": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            },
            "required": ["start", "end"],
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
        let arguments: TraceSearchArguments =
            serde_json::from_value(arguments).map_err(|error| {
                OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
            })?;
        validate_range(arguments.start, arguments.end, self.max_range_seconds)?;
        let limit = arguments.limit.unwrap_or(20).min(50);
        let started = Instant::now();
        let mut query = vec![
            ("start".to_owned(), arguments.start.timestamp().to_string()),
            ("end".to_owned(), arguments.end.timestamp().to_string()),
            ("limit".to_owned(), limit.to_string()),
        ];
        if let Some(service) = &arguments.service {
            query.push(("tags".into(), format!("service.name={service}")));
        }
        if let Some(min_duration) = &arguments.min_duration {
            query.push(("minDuration".into(), min_duration.clone()));
        }
        let query_pairs: Vec<(String, String)> = query;
        let (body, truncated) = fetch_bounded(
            &self.client,
            self.endpoint.clone(),
            &query_pairs,
            cancellation,
            self.max_output_bytes,
        )
        .await?;
        let content = decode_json(&body, truncated, self.max_output_bytes)?;
        let summary = format!(
            "Tempo search returned {} trace(s)",
            content
                .get("traces")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
        );
        Ok(ToolOutput {
            content,
            evidence: EvidenceMeta::new("tempo")
                .with_query(format!("search service={:?}", arguments.service))
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

#[async_trait]
impl Tool for TempoTraceGetTool {
    fn name(&self) -> &str {
        "trace_get"
    }

    fn description(&self) -> &str {
        "Fetch a Tempo trace by ID and return a bounded span summary. Full payload is stored as an artifact."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "trace_id": {"type": "string"}
            },
            "required": ["trace_id"],
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
        let arguments: TraceGetArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::InvalidQuery, error.to_string())
        })?;
        if arguments.trace_id.trim().is_empty()
            || arguments.trace_id.chars().any(|ch| !ch.is_ascii_hexdigit())
        {
            return Err(OpsCodexError::connector(
                ConnectorClass::InvalidQuery,
                "trace_id must be hexadecimal",
            ));
        }
        let started = Instant::now();
        let mut url = self.base.clone();
        url.set_path(&format!(
            "{}/api/traces/{}",
            url.path().trim_end_matches('/'),
            arguments.trace_id
        ));
        let (body, truncated) =
            fetch_bounded(&self.client, url, &[], cancellation, self.max_output_bytes).await?;
        let raw = decode_json(&body, truncated, self.max_output_bytes)?;
        let summary = summarize_trace(&raw);
        Ok(ToolOutput {
            content: json!({
                "trace_id": arguments.trace_id,
                "summary": summary,
                "truncated": truncated,
                "raw": raw,
            }),
            evidence: EvidenceMeta::new("tempo")
                .with_query(format!("trace_get {}", arguments.trace_id))
                .with_duration_ms(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                .with_truncated(truncated)
                .with_summary(summary)
                .with_content_type("application/json"),
        })
    }
}

fn validate_range(start: DateTime<Utc>, end: DateTime<Utc>, max_range_seconds: u64) -> Result<()> {
    if end < start {
        return Err(OpsCodexError::connector(
            ConnectorClass::InvalidQuery,
            "end must be after start",
        ));
    }
    let range = (end - start).num_seconds().unsigned_abs();
    if range > max_range_seconds {
        return Err(OpsCodexError::connector(
            ConnectorClass::InvalidQuery,
            format!("range {range}s exceeds max {max_range_seconds}s"),
        ));
    }
    Ok(())
}

async fn fetch_bounded(
    client: &reqwest::Client,
    url: Url,
    query: &[(String, String)],
    cancellation: CancellationToken,
    max_output_bytes: usize,
) -> Result<(Vec<u8>, bool)> {
    connector::retry_readonly(&cancellation, 3, || {
        let client = client.clone();
        let url = url.clone();
        let query = query.to_vec();
        let cancellation = cancellation.clone();
        async move {
            let request = client.get(url).query(&query);
            let response = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                response = request.send() => response.map_err(|error| {
                    OpsCodexError::connector(ConnectorClass::Unavailable, error.to_string())
                })?,
            };
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(OpsCodexError::connector(
                    ConnectorClass::InvalidQuery,
                    "trace not found",
                ));
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(connector::http_status_error("Tempo", status, &body));
            }
            read_bounded_body(response, cancellation, max_output_bytes).await
        }
    })
    .await
}

fn decode_json(body: &[u8], truncated: bool, max_output_bytes: usize) -> Result<Value> {
    if truncated {
        let (text, _) = truncate_output_with_marker(body, max_output_bytes);
        return Ok(json!({"truncated": true, "raw": text}));
    }
    match serde_json::from_slice(body) {
        Ok(value) => Ok(value),
        Err(_) => {
            let (text, _) = truncate_output(body, max_output_bytes);
            Ok(Value::String(text))
        }
    }
}

fn summarize_trace(value: &Value) -> String {
    let batches = value
        .get("batches")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    format!("Tempo trace with {batches} resource batch(es)")
}
