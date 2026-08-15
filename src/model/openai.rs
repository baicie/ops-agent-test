use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{OpsCodexError, Result};

use super::{
    ModelEvent, ModelEventSink, ModelItem, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    Usage,
};

const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/responses";

pub struct OpenAIResponsesProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    fn request_body(&self, request: ModelRequest) -> Value {
        let input = request
            .input
            .into_iter()
            .map(model_item_json)
            .collect::<Vec<_>>();
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters
                })
            })
            .collect::<Vec<_>>();
        json!({
            "model": self.model,
            "instructions": request.instructions,
            "input": input,
            "tools": tools,
            "stream": true
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAIResponsesProvider {
    async fn complete(
        &self,
        request: ModelRequest,
        sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse> {
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            result = self.client
                .post(&self.endpoint)
                .bearer_auth(&self.api_key)
                .json(&self.request_body(request))
                .send() => result.map_err(|error| OpsCodexError::Model(error.to_string()))?,
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(OpsCodexError::Model(format!("HTTP {status}: {body}")));
        }

        let mut decoder = SseDecoder::default();
        let mut state = ResponseState::default();
        let mut bytes = response.bytes_stream();
        loop {
            let next = tokio::select! {
                _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                next = bytes.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|error| OpsCodexError::Model(error.to_string()))?;
            for data in decoder.push(&chunk)? {
                if data == "[DONE]" {
                    continue;
                }
                let event: Value = serde_json::from_str(&data)
                    .map_err(|error| OpsCodexError::Protocol(error.to_string()))?;
                state.consume(event, &sink)?;
            }
        }
        for data in decoder.finish()? {
            if data != "[DONE]" {
                let event: Value = serde_json::from_str(&data)
                    .map_err(|error| OpsCodexError::Protocol(error.to_string()))?;
                state.consume(event, &sink)?;
            }
        }
        Ok(state.finish())
    }
}

fn model_item_json(item: ModelItem) -> Value {
    match item {
        ModelItem::UserMessage { content } => json!({"role": "user", "content": content}),
        ModelItem::AssistantMessage { content } => {
            json!({"role": "assistant", "content": content})
        }
        ModelItem::ToolCall {
            call_id,
            name,
            arguments,
        } => json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments.to_string()
        }),
        ModelItem::ToolResult { call_id, output } => json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": output.to_string()
        }),
    }
}

#[derive(Default)]
struct ResponseState {
    message: String,
    outputs: Vec<ModelOutput>,
    response_id: Option<String>,
    usage: Usage,
}

impl ResponseState {
    fn consume(&mut self, event: Value, sink: &ModelEventSink) -> Result<()> {
        match event["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                if let Some(delta) = event["delta"].as_str() {
                    self.message.push_str(delta);
                    let _ = sink.send(ModelEvent::MessageDelta(delta.into()));
                }
            }
            "response.output_item.done" => self.consume_output(&event["item"]),
            "response.completed" => self.consume_completed(&event["response"]),
            "error" | "response.failed" | "response.incomplete" => {
                let message = event
                    .pointer("/error/message")
                    .or_else(|| event.pointer("/response/error/message"))
                    .or_else(|| event.pointer("/response/incomplete_details/reason"))
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses API failed");
                return Err(OpsCodexError::Model(message.into()));
            }
            _ => {}
        }
        Ok(())
    }

    fn consume_output(&mut self, item: &Value) {
        if item["type"] == "function_call" {
            self.push_tool_call(item);
        }
    }

    fn consume_completed(&mut self, response: &Value) {
        self.response_id = response["id"].as_str().map(str::to_owned);
        self.usage = serde_json::from_value(response["usage"].clone()).unwrap_or_default();
        if let Some(items) = response["output"].as_array() {
            for item in items {
                if item["type"] == "function_call" {
                    self.push_tool_call(item);
                } else if self.message.is_empty()
                    && item["type"] == "message"
                    && let Some(parts) = item["content"].as_array()
                {
                    for part in parts {
                        if let Some(text) = part["text"].as_str() {
                            self.message.push_str(text);
                        }
                    }
                }
            }
        }
    }

    fn push_tool_call(&mut self, item: &Value) {
        let Some(call_id) = item["call_id"].as_str() else {
            return;
        };
        if self.outputs.iter().any(
            |output| matches!(output, ModelOutput::ToolCall { call_id: id, .. } if id == call_id),
        ) {
            return;
        }
        let arguments = item["arguments"]
            .as_str()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or(Value::Null);
        self.outputs.push(ModelOutput::ToolCall {
            call_id: call_id.into(),
            name: item["name"].as_str().unwrap_or_default().into(),
            arguments,
        });
    }

    fn finish(mut self) -> ModelResponse {
        if !self.message.is_empty() {
            self.outputs.insert(
                0,
                ModelOutput::Message {
                    content: self.message,
                },
            );
        }
        ModelResponse {
            outputs: self.outputs,
            response_id: self.response_id,
            usage: self.usage,
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some((index, separator_len)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..index).collect::<Vec<_>>();
            self.buffer.drain(..separator_len);
            if let Some(data) = frame_data(&frame)? {
                frames.push(data);
            }
        }
        Ok(frames)
    }

    fn finish(&mut self) -> Result<Vec<String>> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }
        let frame = std::mem::take(&mut self.buffer);
        Ok(frame_data(&frame)?.into_iter().collect())
    }
}

fn frame_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2))
        .or_else(|| {
            bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| (index, 4))
        })
}

fn frame_data(frame: &[u8]) -> Result<Option<String>> {
    let frame =
        std::str::from_utf8(frame).map_err(|error| OpsCodexError::Protocol(error.to_string()))?;
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    Ok((!data.is_empty()).then_some(data))
}
