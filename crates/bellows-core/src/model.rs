//! `ModelProvider` — vendor-neutral LLM abstraction.
//!
//! The contract is intentionally narrow: build a request, get either a final
//! response or a stream. Provider-specific concerns (auth, prompt caching,
//! reasoning blocks, vision) are encapsulated in the implementations under
//! `bellows-model` — this crate exposes only the canonical shapes.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::{Message, Result, Role, ToolSchema};

/// One request to a model provider.
///
/// All optional fields default to provider-sensible defaults; implementations
/// must not panic on `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Provider-specific model id (e.g. `"claude-opus-4-7"`, `"gpt-5-thinking"`).
    pub model: String,
    /// Conversation history (excludes the role overlay — that's applied separately).
    pub messages: Vec<Message>,
    /// Merged role to apply as a system overlay. `None` = no system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Tool schemas the model may invoke. Empty = no tools available this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
    /// Soft cap on output tokens. `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature 0.0..=2.0. `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Stop sequences. Empty = none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

/// One non-streaming model response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    /// The assistant message produced (may carry tool_calls).
    pub message: Message,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Token usage if reported by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
}

/// Reason the model finished a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model emitted a complete final answer.
    EndTurn,
    /// Hit `max_tokens` cap.
    MaxTokens,
    /// One of `ModelRequest::stop` matched.
    StopSequence,
    /// Model emitted tool calls and is waiting for results.
    ToolUse,
    /// Provider-specific or unknown stop reason.
    Other,
}

/// Token usage reported by the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Tokens consumed by the input messages + role overlay.
    pub input_tokens: u32,
    /// Tokens generated as output.
    pub output_tokens: u32,
    /// Cached input tokens, if the provider exposes prompt caching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
}

/// Streaming model response. Each item is a partial-response event.
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelStreamEvent>> + Send>>;

/// One streamed delta from a model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    /// A chunk of text content was produced.
    TextDelta {
        /// The text chunk.
        text: String,
    },
    /// The model started a tool call. `arguments` will arrive as
    /// `ToolCallDelta` events and finalise on `EndTurn`.
    ToolCallStart {
        /// Provider-supplied call id.
        id: String,
        /// Tool name.
        name: String,
    },
    /// A chunk of a tool call's `arguments` JSON.
    ToolCallDelta {
        /// Call id this delta belongs to.
        id: String,
        /// Partial JSON string.
        arguments_json: String,
    },
    /// The model is done with this turn.
    EndTurn {
        /// Why it stopped.
        stop_reason: StopReason,
        /// Final usage, if reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<ModelUsage>,
    },
}

/// One model provider connector.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stable provider id (e.g. `"anthropic"`, `"openai"`, `"openrouter"`).
    fn id(&self) -> &str;

    /// Non-streaming completion. Implementations may simulate this on top of
    /// `stream` when the provider has no buffered endpoint.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse>;

    /// Streaming completion. Implementations must always yield a final
    /// `ModelStreamEvent::EndTurn`.
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
}

