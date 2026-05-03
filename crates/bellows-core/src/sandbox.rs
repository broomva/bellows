//! `Sandbox` — the connector trait for tool execution environments.
//!
//! A `Sandbox` provides:
//! - shell execution (`exec`),
//! - filesystem read/write/list,
//! within a configurable isolation boundary. Implementations vary in their
//! isolation guarantees:
//!
//! | Implementation crate | Isolation | Default? |
//! |---|---|---|
//! | `bellows-sandbox-local` (subprocess) | None — runs as the parent process | yes |
//! | `bellows-sandbox-docker` (bollard) | Container | feature-gated |
//! | `bellows-sandbox-e2b` (remote HTTP) | Vendor-managed microVM | feature-gated |
//! | `bellows-sandbox-namespaces` (Linux) | unshare + Landlock + seccomp | feature-gated, Linux-only |
//!
//! The default `bellows-sandbox-local` is honest: same posture as `cargo`,
//! `make`, or Claude Code — the agent runs as you, with your permissions.
//! Docs are explicit about this.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Options applied to a single `Sandbox::exec` invocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecOpts {
    /// Working directory for the process. `None` = sandbox default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment variables to inject. Implementations may merge with their
    /// own allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// Hard timeout in milliseconds. `None` = sandbox default. Implementations
    /// should kill the process group on timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Optional stdin payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
}

/// Result of `Sandbox::exec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    /// Process exit status. `None` if killed before exit (e.g. timeout).
    pub exit_code: Option<i32>,
    /// Captured stdout. Implementations may apply size caps.
    pub stdout: String,
    /// Captured stderr. Implementations may apply size caps.
    pub stderr: String,
    /// True when the process was terminated by the sandbox (timeout, OOM, etc).
    #[serde(default)]
    pub killed: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// One entry produced by `Sandbox::list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    /// Relative or absolute path of the entry, depending on sandbox semantics.
    pub path: String,
    /// `true` if the entry is a directory.
    pub is_dir: bool,
    /// Size in bytes for files; `None` for directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Trait every sandbox connector implements.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Stable identifier for this sandbox instance (for tracing).
    fn name(&self) -> &str;

    /// Run a shell command and capture stdout/stderr.
    async fn exec(&self, cmd: &str, opts: ExecOpts) -> Result<ExecResult>;

    /// Read a file as raw bytes.
    async fn read(&self, path: &str) -> Result<Vec<u8>>;

    /// Write a file. Implementations should create parent directories as needed
    /// and atomically replace existing files where possible.
    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()>;

    /// List a directory non-recursively.
    async fn list(&self, path: &str) -> Result<Vec<DirEntry>>;
}
