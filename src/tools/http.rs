use std::{collections::HashSet, time::Instant};

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

pub struct HttpGetTool {
    client: reqwest::Client,
    allowed_hosts: HashSet<String>,
    max_output_bytes: usize,
}

impl HttpGetTool {
    pub fn new<I, S>(client: reqwest::Client, allowed_hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            client,
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(Into::into)
                .map(|host: String| normalize_host(&host))
                .collect(),
            max_output_bytes: super::MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }

    fn validate_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value)
            .map_err(|error| OpsCodexError::Tool(format!("invalid URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(OpsCodexError::Policy(
                "http_get only permits http and https URLs".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(OpsCodexError::Policy(
                "http_get URLs cannot contain credentials".into(),
            ));
        }
        if url.fragment().is_some() {
            return Err(OpsCodexError::Policy(
                "http_get URLs cannot contain fragments".into(),
            ));
        }
        let host = url
            .host_str()
            .map(normalize_host)
            .ok_or_else(|| OpsCodexError::Policy("http_get URL must include a host".into()))?;
        if !self.allowed_hosts.contains(&host) {
            return Err(OpsCodexError::Policy(format!(
                "http_get host `{host}` is not allowlisted"
            )));
        }
        Ok(url)
    }
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpGetArguments {
    url: String,
}

#[async_trait]
impl Tool for HttpGetTool {
    fn name(&self) -> &str {
        "http_get"
    }

    fn description(&self) -> &str {
        "Send a read-only GET request to an allowlisted service host"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Absolute HTTP or HTTPS URL"}
            },
            "required": ["url"],
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
        let arguments: HttpGetArguments = serde_json::from_value(arguments)
            .map_err(|error| OpsCodexError::Tool(format!("invalid http_get arguments: {error}")))?;
        let url = self.validate_url(&arguments.url)?;
        let started = Instant::now();
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            response = self.client.get(url.clone()).send() => response
                .map_err(|error| OpsCodexError::Tool(format!("HTTP GET failed: {error}")))?,
        };
        if response.url() != &url {
            return Err(OpsCodexError::Policy(
                "http_get does not permit redirects".into(),
            ));
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let (body, stream_truncated) =
            read_bounded_body(response, cancellation.clone(), self.max_output_bytes)
                .await
                .map_err(|error| match error {
                    OpsCodexError::Cancelled => OpsCodexError::Cancelled,
                    other => OpsCodexError::Tool(format!("failed to read HTTP response: {other}")),
                })?;
        let (body_text, text_truncated) = if stream_truncated {
            truncate_output_with_marker(&body, self.max_output_bytes)
        } else {
            truncate_output(&body, self.max_output_bytes)
        };
        let truncated = stream_truncated || text_truncated;
        let body = if !truncated
            && content_type
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        {
            serde_json::from_slice(&body).unwrap_or_else(|_| Value::String(body_text.clone()))
        } else {
            Value::String(body_text)
        };

        Ok(ToolOutput {
            content: json!({"status": status.as_u16(), "body": body}),
            evidence: EvidenceMeta {
                source: "http".into(),
                query: Some(url.to_string()),
                timestamp: Utc::now(),
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                truncated,
            },
        })
    }
}
