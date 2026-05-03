//! `bellows-tool` — built-in tool implementations.
//!
//! Ships:
//! - [`BashTool`] — shell execution via the active sandbox.
//! - [`FsReadTool`], [`FsWriteTool`], [`FsListTool`] — filesystem ops via the active sandbox.
//! - A [`SimpleRegistry`] that fans these out as a [`ToolRegistry`].
//!
//! MCP support lands behind a feature flag in v0.2. The MCP adapter will
//! register every remote MCP tool as an additional `Tool` in the same
//! registry so that the runtime sees one unified tool list.

use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{BellowsError, ExecOpts, Result, Sandbox, Tool, ToolRegistry, ToolSchema};
use serde_json::json;

/// Registry that holds tools in an `Arc<Vec<_>>` and resolves by name.
#[derive(Clone, Default)]
pub struct SimpleRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for SimpleRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self.tools.iter().map(|t| t.schema().name).collect();
        f.debug_struct("SimpleRegistry").field("tools", &names).finish()
    }
}

impl SimpleRegistry {
    /// Empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Builder-style helper.
    #[must_use]
    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }
}

impl ToolRegistry for SimpleRegistry {
    fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.schema().name == name).cloned()
    }
}

// ── Built-in tools ───────────────────────────────────────────────────────────

/// Run a shell command in the active sandbox.
#[derive(Debug, Default, Clone, Copy)]
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".to_string(),
            description: "Execute a shell command in the agent's sandbox and return stdout/stderr.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["cmd"],
                "properties": {
                    "cmd":  { "type": "string", "description": "Shell command to run." },
                    "cwd":  { "type": "string", "description": "Optional working directory (relative to workspace)." },
                    "timeout_ms": { "type": "integer", "minimum": 0 }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value, sandbox: &dyn Sandbox) -> Result<serde_json::Value> {
        let cmd = args
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| BellowsError::Tool {
                name: "bash".to_string(),
                reason: "missing required `cmd` argument".to_string(),
            })?;
        let opts = ExecOpts {
            cwd: args.get("cwd").and_then(|v| v.as_str().map(String::from)),
            env: Vec::new(),
            timeout_ms: args.get("timeout_ms").and_then(serde_json::Value::as_u64),
            stdin: None,
        };
        let r = sandbox.exec(cmd, opts).await?;
        Ok(serde_json::to_value(r)?)
    }
}

/// Read a file from the active sandbox as UTF-8.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsReadTool;

#[async_trait]
impl Tool for FsReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_read".to_string(),
            description: "Read a file from the agent's sandbox as UTF-8 text.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value, sandbox: &dyn Sandbox) -> Result<serde_json::Value> {
        let path = args.get("path").and_then(serde_json::Value::as_str).ok_or_else(|| BellowsError::Tool {
            name: "fs_read".to_string(),
            reason: "missing required `path` argument".to_string(),
        })?;
        let bytes = sandbox.read(path).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(json!({ "path": path, "content": text }))
    }
}

/// Write a file in the active sandbox.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsWriteTool;

#[async_trait]
impl Tool for FsWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_write".to_string(),
            description: "Write a file in the agent's sandbox. Creates parent dirs as needed.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path":    { "type": "string" },
                    "content": { "type": "string" }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value, sandbox: &dyn Sandbox) -> Result<serde_json::Value> {
        let path = args.get("path").and_then(serde_json::Value::as_str).ok_or_else(|| BellowsError::Tool {
            name: "fs_write".to_string(),
            reason: "missing required `path` argument".to_string(),
        })?;
        let content = args.get("content").and_then(serde_json::Value::as_str).ok_or_else(|| BellowsError::Tool {
            name: "fs_write".to_string(),
            reason: "missing required `content` argument".to_string(),
        })?;
        sandbox.write(path, content.as_bytes()).await?;
        Ok(json!({ "path": path, "written": content.len() }))
    }
}

/// List a directory in the active sandbox.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsListTool;

#[async_trait]
impl Tool for FsListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs_list".to_string(),
            description: "List a directory in the agent's sandbox.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value, sandbox: &dyn Sandbox) -> Result<serde_json::Value> {
        let path = args.get("path").and_then(serde_json::Value::as_str).ok_or_else(|| BellowsError::Tool {
            name: "fs_list".to_string(),
            reason: "missing required `path` argument".to_string(),
        })?;
        let entries = sandbox.list(path).await?;
        Ok(json!({ "path": path, "entries": entries }))
    }
}
