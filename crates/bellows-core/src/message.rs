//! Conversation primitives: messages, roles, tool calls.
//!
//! These types form the wire-stable representation of a turn in a
//! [`Session`](crate::Session). They are intentionally simple and JSON-friendly
//! so they can flow through provider adapters, MCP, and HTTP responses without
//! transformation.

use serde::{Deserialize, Serialize};

/// Role of a single message in the conversation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgRole {
    /// System-prompt content. Usually only the first message in a session.
    /// Note: [`Role`](crate::Role) overlays produced via `Role::merge` are
    /// applied at request-build time and **never persisted as system messages**.
    System,
    /// User-supplied content (the agent's caller).
    User,
    /// Assistant content (the model's reply).
    Assistant,
    /// Tool result content, attached to a prior assistant tool call.
    Tool,
}

/// A single message in a session's conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Originator role for this message.
    pub role: MsgRole,
    /// Free-text content. May be empty when `tool_calls` carries the payload.
    #[serde(default)]
    pub content: String,
    /// Tool calls emitted by the assistant in this turn. Empty for non-assistant turns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool results attached to this turn. Empty for non-tool turns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
}

impl Message {
    /// Convenience constructor for a plain user message.
    #[must_use]
    pub fn user<S: Into<String>>(content: S) -> Self {
        Self {
            role: MsgRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// Convenience constructor for a plain assistant message.
    #[must_use]
    pub fn assistant<S: Into<String>>(content: S) -> Self {
        Self {
            role: MsgRole::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }
}

/// A tool invocation requested by the model in an assistant turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-supplied unique id for this tool call (used to correlate results).
    pub id: String,
    /// Logical name of the tool the model wants to invoke.
    pub name: String,
    /// JSON arguments that conform to the tool's `Tool::schema()`.
    pub arguments: serde_json::Value,
}

/// The result of executing a `ToolCall`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Echoes `ToolCall::id` so the model can correlate.
    pub call_id: String,
    /// JSON output from the tool.
    pub output: serde_json::Value,
    /// True when the tool returned an error rather than a normal result.
    #[serde(default)]
    pub is_error: bool,
}
