use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{OpsCodexError, Result};

use super::{ModelEvent, ModelEventSink, ModelOutput, ModelProvider, ModelRequest, ModelResponse};

#[derive(Clone)]
pub struct FakeModelProvider {
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl FakeModelProvider {
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl ModelProvider for FakeModelProvider {
    async fn complete(
        &self,
        request: ModelRequest,
        sink: ModelEventSink,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(OpsCodexError::Cancelled);
        }
        self.requests.lock().await.push(request);
        let response = self
            .responses
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| OpsCodexError::Model("fake response queue exhausted".into()))?;
        for output in &response.outputs {
            if let ModelOutput::Message { content } = output {
                let _ = sink.send(ModelEvent::MessageDelta(content.clone()));
            }
        }
        Ok(response)
    }
}
