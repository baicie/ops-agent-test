mod connector;
mod docker_logs;
mod exec;
mod fake;
mod http;
mod loki;
mod promql;
mod registry;
mod tempo;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Result, runtime::EvidenceMeta};

pub use docker_logs::DockerLogsTool;
pub use exec::ExecTool;
pub use fake::FakeTool;
pub use http::HttpGetTool;
pub use loki::LokiLogQueryTool;
pub use promql::PromqlTool;
pub use registry::ToolRegistry;
pub use tempo::{TempoTraceGetTool, TempoTraceSearchTool};

pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    Safe,
    Ask,
    Forbidden,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub content: Value,
    pub evidence: EvidenceMeta,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn schema(&self) -> Value;

    fn risk(&self) -> ToolRisk;

    async fn execute(
        &self,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput>;
}

pub(crate) fn truncate_output(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    truncate_output_inner(bytes, max_bytes, false)
}

pub(crate) fn truncate_output_with_marker(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    truncate_output_inner(bytes, max_bytes, true)
}

fn truncate_output_inner(bytes: &[u8], max_bytes: usize, force_marker: bool) -> (String, bool) {
    const MARKER: &str = "\n[output truncated]";
    let decoded = String::from_utf8_lossy(bytes);
    let max_bytes = max_bytes.max(1);
    if !force_marker && decoded.len() <= max_bytes {
        return (decoded.into_owned(), false);
    }

    if max_bytes <= MARKER.len() {
        return (MARKER[..max_bytes].to_owned(), true);
    }
    let mut content_len = max_bytes - MARKER.len();
    while !decoded.is_char_boundary(content_len) {
        content_len -= 1;
    }
    let mut output = decoded[..content_len].to_owned();
    output.push_str(MARKER);
    (output, true)
}

pub(crate) async fn read_bounded_body(
    response: reqwest::Response,
    cancellation: CancellationToken,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool)> {
    let max_bytes = max_bytes.max(1);
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .map(|length| (length as usize).min(max_bytes))
            .unwrap_or(0),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(crate::OpsCodexError::Cancelled),
        chunk = stream.next() => chunk,
    } {
        let chunk = chunk.map_err(|error| crate::OpsCodexError::Tool(error.to_string()))?;
        let remaining = max_bytes.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}
