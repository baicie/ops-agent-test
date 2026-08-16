use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Result;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelItem {
    UserMessage {
        content: String,
    },
    AssistantMessage {
        content: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        output: Value,
    },
}

impl ModelItem {
    pub fn is_tool_result(&self, expected_call_id: &str) -> bool {
        matches!(self, Self::ToolResult { call_id, .. } if call_id == expected_call_id)
    }

    pub fn message_contains(&self, needle: &str) -> bool {
        matches!(self, Self::AssistantMessage { content } if content.contains(needle))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelRequest {
    pub instructions: String,
    pub input: Vec<ModelItem>,
    pub tools: Vec<ToolSchema>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelOutput {
    Message {
        content: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelResponse {
    pub outputs: Vec<ModelOutput>,
    pub response_id: Option<String>,
    pub usage: Usage,
}

impl ModelResponse {
    pub fn new(outputs: Vec<ModelOutput>) -> Self {
        Self {
            outputs,
            response_id: None,
            usage: Usage::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    MessageDelta(String),
}

pub type ModelEventSink = mpsc::UnboundedSender<ModelEvent>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    pub parallel_calls: bool,
    pub usage: bool,
    pub reasoning_control: bool,
    pub continuation: bool,
    pub request_idempotency: bool,
    pub structured_output: bool,
}

impl ModelCapabilities {
    pub fn openai_responses() -> Self {
        Self {
            streaming: true,
            tool_calls: true,
            parallel_calls: true,
            usage: true,
            reasoning_control: true,
            continuation: false,
            request_idempotency: false,
            structured_output: false,
        }
    }

    pub fn fake() -> Self {
        Self {
            streaming: true,
            tool_calls: true,
            parallel_calls: false,
            usage: false,
            reasoning_control: false,
            continuation: false,
            request_idempotency: false,
            structured_output: false,
        }
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::fake()
    }

    async fn complete(
        &self,
        request: ModelRequest,
        sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse>;
}
