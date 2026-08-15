use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    OpsCodexError, Result,
    runtime::EvidenceMeta,
    tools::{
        Tool, ToolOutput, ToolRisk, read_bounded_body, truncate_output, truncate_output_with_marker,
    },
};

pub struct PromqlTool {
    client: reqwest::Client,
    endpoint: Url,
    max_output_bytes: usize,
}

impl PromqlTool {
    pub fn new(client: reqwest::Client, base_url: impl AsRef<str>) -> Result<Self> {
        let mut endpoint = Url::parse(base_url.as_ref())
            .map_err(|error| OpsCodexError::Tool(format!("invalid Prometheus URL: {error}")))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(OpsCodexError::Tool(
                "Prometheus URL must use http or https".into(),
            ));
        }
        if endpoint.host_str().is_none() {
            return Err(OpsCodexError::Tool(
                "Prometheus URL must include a host".into(),
            ));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let path = format!("{}/api/v1/query", endpoint.path().trim_end_matches('/'));
        endpoint.set_path(&path);
        Ok(Self {
            client,
            endpoint,
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
struct PromqlArguments {
    query: String,
}

#[async_trait]
impl Tool for PromqlTool {
    fn name(&self) -> &str {
        "promql_query"
    }

    fn description(&self) -> &str {
        "Run an instant PromQL query against the configured Prometheus server"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "PromQL expression"}
            },
            "required": ["query"],
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
        let arguments: PromqlArguments = serde_json::from_value(arguments).map_err(|error| {
            OpsCodexError::Tool(format!("invalid promql_query arguments: {error}"))
        })?;
        if arguments.query.trim().is_empty() {
            return Err(OpsCodexError::Tool(
                "promql_query query cannot be empty".into(),
            ));
        }

        let started = Instant::now();
        let request = self
            .client
            .get(self.endpoint.clone())
            .query(&[("query", &arguments.query)]);
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            response = request.send() => response
                .map_err(|error| OpsCodexError::Tool(format!("Prometheus request failed: {error}")))?,
        };
        let status = response.status();
        if !status.is_success() {
            return Err(OpsCodexError::Tool(format!(
                "Prometheus returned HTTP {status}"
            )));
        }
        let (body, truncated) =
            read_bounded_body(response, cancellation, self.max_output_bytes).await?;
        let (text, text_truncated) = if truncated {
            truncate_output_with_marker(&body, self.max_output_bytes)
        } else {
            truncate_output(&body, self.max_output_bytes)
        };
        let truncated = truncated || text_truncated;
        let content = if truncated {
            json!({"truncated": true, "raw": text})
        } else {
            serde_json::from_slice::<Value>(&body).map_err(|error| {
                OpsCodexError::Tool(format!("invalid Prometheus response: {error}"))
            })?
        };

        Ok(ToolOutput {
            content,
            evidence: EvidenceMeta {
                source: "prometheus".into(),
                query: Some(arguments.query),
                timestamp: Utc::now(),
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                truncated,
            },
        })
    }
}
