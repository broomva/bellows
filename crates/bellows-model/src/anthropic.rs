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
//! Streaming consumes the Messages API SSE protocol described at
//! <https://docs.anthropic.com/en/api/messages-streaming>: a sequence of
//! `message_start` → (`content_block_start` → `content_block_delta`* →
//! `content_block_stop`)* → `message_delta` → `message_stop`. We project
//! each upstream event into the `ModelStreamEvent` taxonomy (TextDelta /
//! ToolCallStart / ToolCallDelta / EndTurn).

use std::{collections::HashMap, pin::Pin};

use async_trait::async_trait;
use bellows_core::{
    BellowsError, Message, ModelProvider, ModelRequest, ModelResponse, ModelStream, ModelUsage,
    MsgRole, Result, StopReason, ToolCall, model::ModelStreamEvent,
};
use eventsource_stream::Eventsource;
use futures::{StreamExt, stream};
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
        let body = build_request_body(&request, false);
        let url = format!("{}/v1/messages", self.base_url);

        let req = self.apply_auth(
            self.client
                .post(&url)
                .header("anthropic-version", &self.api_version)
                .header("content-type", "application/json"),
        );

        let resp = req.json(&body).send().await.map_err(map_transport)?;

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
        let body = build_request_body(&request, true);
        let url = format!("{}/v1/messages", self.base_url);

        let req = self.apply_auth(
            self.client
                .post(&url)
                .header("anthropic-version", &self.api_version)
                .header("accept", "text/event-stream")
                .header("content-type", "application/json"),
        );

        let resp = req.json(&body).send().await.map_err(map_transport)?;
        let status = resp.status();

        if !status.is_success() {
            // Drain the body up to a sane cap so we surface a useful preview.
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| BellowsError::Model(format!("read body: {e}")))?;
            let preview = String::from_utf8_lossy(&bytes);
            return Err(BellowsError::Model(format!("HTTP {status}: {preview}")));
        }

        let byte_stream = resp.bytes_stream();
        let events = byte_stream.eventsource();
        let bellows_stream = stream::unfold(StreamState::new(events), move |mut st| async move {
            st.next_step().await.map(|item| (item, st))
        });

        let s: ModelStream = Box::pin(bellows_stream)
            as Pin<Box<dyn futures::Stream<Item = Result<ModelStreamEvent>> + Send>>;
        Ok(s)
    }
}

impl AnthropicProvider {
    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AnthropicAuth::ApiKey(k) => req.header("x-api-key", k),
            AnthropicAuth::OAuthBearer(t) => req
                .header("authorization", format!("Bearer {t}"))
                .header("anthropic-beta", &self.oauth_beta),
        }
    }
}

fn map_transport(e: reqwest::Error) -> BellowsError {
    let mut msg = format!("transport: {e}");
    let mut src: &dyn std::error::Error = &e;
    while let Some(s) = src.source() {
        msg.push_str(" -> ");
        msg.push_str(&s.to_string());
        src = s;
    }
    BellowsError::Model(msg)
}

// ── SSE state machine ────────────────────────────────────────────────────────

/// Decoded shape of one Anthropic SSE event payload (the part inside
/// `data: ...`). We only model the variants we project; everything else
/// (`message_start`, `content_block_stop`, `ping`) is consumed silently.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicSseEvent {
    /// New content block began. We need the index → tool-use id mapping
    /// for subsequent `input_json_delta` events.
    ContentBlockStart {
        index: u32,
        content_block: AnthropicSseContentBlock,
    },
    /// Either text or tool-input chunk for an open block.
    ContentBlockDelta {
        index: u32,
        delta: AnthropicSseDelta,
    },
    /// Carries the final stop reason — buffered until message_stop.
    MessageDelta {
        delta: AnthropicSseMessageDelta,
        #[serde(default)]
        usage: Option<AnthropicSseUsageDelta>,
    },
    /// Terminal event for the request.
    MessageStop,
    /// Streaming-side error (e.g. overloaded mid-flight). Surfaced as
    /// `BellowsError::Model`.
    Error { error: AnthropicSseError },
    /// Catch-all for `message_start`, `content_block_stop`, `ping`, and
    /// any future variants. Silently ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicSseContentBlock {
    /// Text content block (no payload at start — text arrives via deltas).
    Text {
        #[serde(default)]
        text: String,
    },
    /// Tool-use content block. `id` + `name` are emitted exactly here;
    /// `input` arrives as `input_json_delta`s on subsequent events.
    ToolUse { id: String, name: String },
    /// Anything else (thinking, vision, …) — ignored for now.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicSseDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseUsageDelta {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseError {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Inner state for the SSE → ModelStreamEvent pump.
///
/// The unfold loop calls `next_step` repeatedly. Each call drives the
/// upstream byte stream forward until it can emit exactly one
/// `ModelStreamEvent` (or `None` to terminate). State carries:
///
/// - `events`: the live SSE source.
/// - `tool_block_ids`: index → tool_use id, populated on
///   `content_block_start { type: tool_use }` so subsequent
///   `input_json_delta` events know which call they belong to.
/// - Buffered stop reason + usage from `message_delta`, emitted on
///   `message_stop` together as one `EndTurn`.
/// - `done`: latched once we emit the terminal `EndTurn` to make the
///   stream behave like a one-shot iterator afterwards.
struct StreamState<S> {
    events: S,
    tool_block_ids: HashMap<u32, String>,
    pending_stop_reason: Option<StopReason>,
    pending_usage: Option<ModelUsage>,
    done: bool,
}

impl<S> StreamState<S> {
    fn new(events: S) -> Self {
        Self {
            events,
            tool_block_ids: HashMap::new(),
            pending_stop_reason: None,
            pending_usage: None,
            done: false,
        }
    }
}

impl<S, E> StreamState<S>
where
    S: futures::Stream<Item = std::result::Result<eventsource_stream::Event, E>>
        + Send
        + Unpin
        + 'static,
    E: std::fmt::Display,
{
    /// Drive the SSE stream until the next `ModelStreamEvent` is ready
    /// to emit. Returns `None` when the upstream is exhausted (or after
    /// we've emitted the terminal `EndTurn`).
    async fn next_step(&mut self) -> Option<Result<ModelStreamEvent>> {
        if self.done {
            return None;
        }
        loop {
            let next = self.events.next().await?;
            match next {
                Err(e) => {
                    self.done = true;
                    return Some(Err(BellowsError::Model(format!("sse transport: {e}"))));
                }
                Ok(ev) => {
                    if !ev.data.is_empty() {
                        match serde_json::from_str::<AnthropicSseEvent>(&ev.data) {
                            Err(e) => {
                                // Unknown / malformed payloads are skipped —
                                // this matches Anthropic's forward-compat
                                // expectation.
                                tracing::debug!(error = %e, raw = %ev.data, "anthropic sse: skipping undecodable event");
                            }
                            Ok(decoded) => {
                                if let Some(out) = self.project(decoded) {
                                    return Some(out);
                                }
                                // No emit — keep pumping.
                            }
                        }
                    }
                }
            }
        }
    }

    /// Project one decoded SSE event into 0-or-1 ModelStreamEvent.
    /// Returns `None` to mean "absorbed, keep pumping".
    fn project(&mut self, ev: AnthropicSseEvent) -> Option<Result<ModelStreamEvent>> {
        match ev {
            AnthropicSseEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                AnthropicSseContentBlock::Text { text } => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(Ok(ModelStreamEvent::TextDelta { text }))
                    }
                }
                AnthropicSseContentBlock::ToolUse { id, name } => {
                    self.tool_block_ids.insert(index, id.clone());
                    Some(Ok(ModelStreamEvent::ToolCallStart { id, name }))
                }
                AnthropicSseContentBlock::Other => None,
            },
            AnthropicSseEvent::ContentBlockDelta { index, delta } => match delta {
                AnthropicSseDelta::TextDelta { text } => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(Ok(ModelStreamEvent::TextDelta { text }))
                    }
                }
                AnthropicSseDelta::InputJsonDelta { partial_json } => {
                    let id = self.tool_block_ids.get(&index)?.clone();
                    Some(Ok(ModelStreamEvent::ToolCallDelta {
                        id,
                        arguments_json: partial_json,
                    }))
                }
                AnthropicSseDelta::Other => None,
            },
            AnthropicSseEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason {
                    self.pending_stop_reason = Some(parse_stop_reason(&reason));
                }
                if let Some(u) = usage {
                    // message_delta usage events overlay the prior input_tokens
                    // (which were sent on message_start, an event we drop) —
                    // merge instead of overwriting.
                    let merged = self.pending_usage.take().unwrap_or_default();
                    self.pending_usage = Some(ModelUsage {
                        input_tokens: u.input_tokens.unwrap_or(merged.input_tokens),
                        output_tokens: u.output_tokens.unwrap_or(merged.output_tokens),
                        cached_input_tokens: u
                            .cache_read_input_tokens
                            .or(merged.cached_input_tokens),
                    });
                }
                None
            }
            AnthropicSseEvent::MessageStop => {
                self.done = true;
                let stop_reason = self.pending_stop_reason.unwrap_or(StopReason::EndTurn);
                let usage = self.pending_usage.take();
                Some(Ok(ModelStreamEvent::EndTurn { stop_reason, usage }))
            }
            AnthropicSseEvent::Error { error } => {
                self.done = true;
                let kind = error.kind.unwrap_or_else(|| "unknown".to_string());
                let msg = error
                    .message
                    .unwrap_or_else(|| "anthropic stream error".to_string());
                Some(Err(BellowsError::Model(format!(
                    "anthropic stream {kind}: {msg}"
                ))))
            }
            AnthropicSseEvent::Unknown => None,
        }
    }
}

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        _ => StopReason::Other,
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
    /// Enables Anthropic's SSE streaming protocol when `true`. Skipped on
    /// the wire for non-streaming requests so we don't disturb the
    /// existing `complete()` shape.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
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

fn build_request_body(req: &ModelRequest, stream: bool) -> AnthropicMessageRequest<'_> {
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
        stream,
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
