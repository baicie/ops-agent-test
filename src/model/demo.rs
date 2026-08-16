use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{OpsCodexError, Result};

use super::{
    ModelCapabilities, ModelEvent, ModelEventSink, ModelItem, ModelOutput, ModelProvider,
    ModelRequest, ModelResponse,
};

#[derive(Default)]
pub struct DemoModelProvider;

#[async_trait]
impl ModelProvider for DemoModelProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::fake()
    }

    async fn complete(
        &self,
        request: ModelRequest,
        sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        let current_turn = request
            .input
            .iter()
            .rposition(|item| matches!(item, ModelItem::UserMessage { .. }))
            .map_or(request.input.as_slice(), |index| &request.input[index..]);
        if !current_turn
            .iter()
            .any(|item| matches!(item, ModelItem::ToolResult { .. }))
            && request.tools.iter().any(|tool| tool.name == "http_get")
        {
            let message = "I'll inspect the service health endpoint first.";
            let _ = sink.send(ModelEvent::MessageDelta(message.into()));
            return Ok(ModelResponse::new(vec![
                ModelOutput::Message {
                    content: message.into(),
                },
                ModelOutput::ToolCall {
                    call_id: format!("call_{}", Uuid::now_v7()),
                    name: "http_get".into(),
                    arguments: json!({"url": "http://localhost:8080/health"}),
                },
            ]));
        }

        let evidence = current_turn
            .iter()
            .rev()
            .find_map(|item| match item {
                ModelItem::ToolResult { output, .. } => Some(output.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "No tool evidence was available.".into());
        let answer = serde_json::json!({
            "summary": "The local diagnostic probe completed.",
            "claims": [{
                "kind": "observed",
                "statement": format!("http_get returned: {evidence}"),
                "evidence_ids": [],
                "confidence": "medium"
            }],
            "recommended_actions": [
                "Start the demo order-service if the probe failed.",
                "Use a Responses-compatible provider for autonomous multi-source investigation."
            ],
            "limitations": ["Local fake-model mode only inspected the health endpoint."]
        })
        .to_string();
        let _ = sink.send(ModelEvent::MessageDelta(answer.clone()));
        Ok(ModelResponse::new(vec![ModelOutput::Message {
            content: answer,
        }]))
    }
}
