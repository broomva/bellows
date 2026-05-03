//! Anthropic Messages API provider.
//!
//! Hand-rolled `reqwest`-based connector. Two auth modes:
//!
//! - **API key** (`AnthropicAuth::ApiKey`): `x-api-key` header. The standard
//!   path for `sk-ant-api03-...` keys.
//! - **OAuth bearer** (`AnthropicAuth::OAuthBearer`): `Authorization: Bearer`
//!   header. Used for Claude Code subscription tokens (`sk-ant-oat01-...`).
//!   Adds `anthropic-beta: oauth-2025-04-20` per Claude Code's wire shape.
//!
//! v0.1 ships non-streaming `complete` only. Streaming + tool-use loops
//! arrive in v0.2 alongside the proc-macro and `bellows-build` work.

use std::pin::Pin;

use async_trait::async_trait;
use bellows_core::{
    BellowsError, Message, ModelProvider, ModelRequest, ModelResponse, ModelStream, ModelUsage,
    MsgRole, Result, StopReason, ToolCall, model::ModelStreamEvent,
};
use futures::stream;
use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_API_VERSION: &str = "2023-06-01";
const DEFAULT_OAUTH_BETA: &str = "oauth-2025-04-20";

/// How to authenticate against Anthropic's API.
#[derive(Debug, Clone)]
pub enum AnthropicAuth {
    /// Standard API key. Sent as `x-api-key` header.
    ApiKey(String),
    /// Claude Code subscription OAuth token. Sent as `Authorization: Bearer`
    /// with the `anthropic-beta: oauth-...` header.
    OAuthBearer(String),
}

impl AnthropicAuth {
    /// Read auth from the environment. Prefers `ANTHROPIC_API_KEY`, falls
    /// back to `CLAUDE_CODE_OAUTH_TOKEN`. Returns `None` if neither is set.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
            if !k.is_empty() {
                return Some(Self::ApiKey(k));
            }
        }
        if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
            if !t.is_empty() {
                return Some(Self::OAuthBearer(t));
            }
        }
        None
    }
}

/// Anthropic Messages API provider.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    auth: AnthropicAuth,
    base_url: String,
    api_version: String,
    oauth_beta: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Build a provider with explicit auth and default endpoints.
    #[must_use]
    pub fn new(auth: AnthropicAuth) -> Self {
        Self {
            auth,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_version: DEFAULT_API_VERSION.to_string(),
            oauth_beta: DEFAULT_OAUTH_BETA.to_string(),
            client: reqwest::Client::builder()
                .user_agent(concat!("bellows/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build a provider from environment variables. Returns
    /// `BellowsError::Config` if no usable auth is found.
    pub fn from_env() -> Result<Self> {
        let auth = AnthropicAuth::from_env().ok_or_else(|| {
            BellowsError::Config(
                "no Anthropic credentials found — set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN"
                    .to_string(),
            )
        })?;
        Ok(Self::new(auth))
    }

    /// Override the API base URL (for proxies / Bedrock-style relays).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = build_request_body(&request);
        let url = format!("{}/v1/messages", self.base_url);

        let mut req = self
            .client
            .post(&url)
            .header("anthropic-version", &self.api_version)
            .header("content-type", "application/json");

        match &self.auth {
            AnthropicAuth::ApiKey(k) => {
                req = req.header("x-api-key", k);
            }
            AnthropicAuth::OAuthBearer(t) => {
                req = req
                    .header("authorization", format!("Bearer {t}"))
                    .header("anthropic-beta", &self.oauth_beta);
            }
        }

        let resp = req.json(&body).send().await.map_err(|e| {
            let mut msg = format!("transport: {e}");
            let mut src: &dyn std::error::Error = &e;
            while let Some(s) = src.source() {
                msg.push_str(" -> ");
                msg.push_str(&s.to_string());
                src = s;
            }
            BellowsError::Model(msg)
        })?;

        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| BellowsError::Model(format!("read body: {e}")))?;

        if !status.is_success() {
            let preview = String::from_utf8_lossy(&bytes);
            return Err(BellowsError::Model(format!("HTTP {status}: {preview}")));
        }

        let parsed: AnthropicMessageResponse = serde_json::from_slice(&bytes)
            .map_err(|e| BellowsError::Model(format!("decode: {e}")))?;

        Ok(parsed.into_response())
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        // v0.1: non-streaming under the hood, framed as one TextDelta + EndTurn.
        // Real SSE streaming + tool-use deltas land in v0.2.
        let resp = self.complete(request).await?;
        let text = resp.message.content.clone();
        let stop = resp.stop_reason;
        let usage = resp.usage.clone();
        let events = vec![
            Ok(ModelStreamEvent::TextDelta { text }),
            Ok(ModelStreamEvent::EndTurn {
                stop_reason: stop,
                usage,
            }),
        ];
        let s: ModelStream = Box::pin(stream::iter(events))
            as Pin<Box<dyn futures::Stream<Item = Result<ModelStreamEvent>> + Send>>;
        Ok(s)
    }
}

// ── Wire shapes ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct AnthropicMessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicWireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct AnthropicWireMessage {
    role: &'static str,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageResponse {
    #[serde(default)]
    content: Vec<AnthropicResponseBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

fn build_request_body(req: &ModelRequest) -> AnthropicMessageRequest<'_> {
    AnthropicMessageRequest {
        model: &req.model,
        max_tokens: req.max_tokens.unwrap_or(1024),
        messages: req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, MsgRole::System)) // system goes into the `system` field
            .map(message_to_wire)
            .collect(),
        system: req.role.as_ref().and_then(bellows_core::Role::render),
        temperature: req.temperature,
        stop_sequences: req.stop.clone(),
        tools: req
            .tools
            .iter()
            .map(|schema| {
                serde_json::json!({
                    "name":         schema.name,
                    "description":  schema.description,
                    "input_schema": schema.parameters,
                })
            })
            .collect(),
    }
}

fn message_to_wire(m: &Message) -> AnthropicWireMessage {
    let role = match m.role {
        MsgRole::User | MsgRole::Tool => "user",
        MsgRole::Assistant => "assistant",
        MsgRole::System => "user", // filtered out above; defensive fallback
    };
    let mut content = Vec::new();
    if !m.content.is_empty() {
        content.push(AnthropicContentBlock::Text {
            text: m.content.clone(),
        });
    }
    // Re-emit assistant tool_use blocks so Anthropic can correlate the
    // upcoming tool_result blocks back to their originating tool_use ids.
    for tc in &m.tool_calls {
        content.push(AnthropicContentBlock::ToolUse {
            id: tc.id.clone(),
            name: tc.name.clone(),
            input: tc.arguments.clone(),
        });
    }
    for tr in &m.tool_results {
        content.push(AnthropicContentBlock::ToolResult {
            tool_use_id: tr.call_id.clone(),
            content: tr.output.to_string(),
            is_error: if tr.is_error { Some(true) } else { None },
        });
    }
    AnthropicWireMessage { role, content }
}

impl AnthropicMessageResponse {
    fn into_response(self) -> ModelResponse {
        let mut text_out = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for block in self.content {
            match block {
                AnthropicResponseBlock::Text { text } => text_out.push_str(&text),
                AnthropicResponseBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                AnthropicResponseBlock::Unknown => {}
            }
        }
        let stop_reason = match self.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_use") => StopReason::ToolUse,
            _ => StopReason::Other,
        };
        let usage = self.usage.map(|u| ModelUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_input_tokens: u.cache_read_input_tokens,
        });
        ModelResponse {
            message: Message {
                role: MsgRole::Assistant,
                content: text_out,
                tool_calls,
                tool_results: Vec::new(),
            },
            stop_reason,
            usage,
        }
    }
}
