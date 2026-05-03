//! In-process echo provider used by tests and examples without network access.

use std::pin::Pin;

use async_trait::async_trait;
use bellows_core::{
    Message, ModelProvider, ModelRequest, ModelResponse, ModelStream, Result, StopReason,
};
use futures::stream;

/// A trivial in-process provider that echoes the last user message.
#[derive(Debug, Clone, Default)]
pub struct MockProvider;

#[async_trait]
impl ModelProvider for MockProvider {
    fn id(&self) -> &'static str {
        "mock"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, bellows_core::MsgRole::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(ModelResponse {
            message: Message::assistant(format!("[mock echo] {last_user}")),
            stop_reason: StopReason::EndTurn,
            usage: None,
        })
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        let resp = self.complete(request).await?;
        let event = bellows_core::model::ModelStreamEvent::TextDelta {
            text: resp.message.content.clone(),
        };
        let end = bellows_core::model::ModelStreamEvent::EndTurn {
            stop_reason: StopReason::EndTurn,
            usage: None,
        };
        let s: ModelStream = Box::pin(stream::iter(vec![Ok(event), Ok(end)]))
            as Pin<
                Box<
                    dyn futures::Stream<Item = Result<bellows_core::model::ModelStreamEvent>>
                        + Send,
                >,
            >;
        Ok(s)
    }
}
