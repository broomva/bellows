//! `Tool` and `ToolRegistry` — the surface the model invokes.
//!
//! The runtime presents a `ToolRegistry` to the model. The registry resolves
//! tool names to `Tool` implementations. When the model emits a `ToolCall`,
//! the runtime fetches the tool, validates arguments, executes it (passing a
//! `Sandbox` reference for FS/exec needs), and appends the resulting
//! `ToolResult` to the session.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Result, Sandbox};

/// JSON Schema describing a tool's argument shape.
///
/// This is the structure surfaced to the model so it can call the tool
/// correctly. Schemas are stable JSON values, not Rust types — implementations
/// generate them once at construction time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Logical tool name. Stable across versions.
    pub name: String,
    /// Free-text description shown to the model.
    pub description: String,
    /// JSON Schema (draft-2020-12) describing the `arguments` object.
    pub parameters: serde_json::Value,
}

/// A single tool the agent can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool schema (name + description + parameter shape) shown to the model.
    fn schema(&self) -> ToolSchema;

    /// Execute the tool with a JSON `arguments` object that conforms to
    /// `self.schema().parameters`. Implementations should validate and return
    /// `BellowsError::Tool { .. }` on bad input rather than panic.
    ///
    /// `sandbox` is provided for tools that need to shell out, read files, or
    /// otherwise touch the host. Tools that are pure (e.g. `now()`,
    /// `random()`, MCP-mediated) may ignore it.
    async fn invoke(
        &self,
        arguments: serde_json::Value,
        sandbox: &dyn Sandbox,
    ) -> Result<serde_json::Value>;
}

/// Resolves tool names to `Tool` implementations.
///
/// Registries are expected to be cheap to clone (typically `Arc`-backed).
pub trait ToolRegistry: Send + Sync {
    /// All tools currently registered, in stable order.
    fn list(&self) -> Vec<Arc<dyn Tool>>;

    /// Look up a tool by name. Returns `None` if absent.
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
}
