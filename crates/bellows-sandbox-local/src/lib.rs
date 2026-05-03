//! `bellows-sandbox-local` — local-process sandbox.
//!
//! Spawns commands as subprocesses with a configurable working directory,
//! environment-variable allowlist, and per-call timeout. **No isolation**
//! beyond cwd/env scoping — the agent runs as the parent process. This is
//! the right choice for developer tools where the user already trusts the
//! binary; it is the wrong choice for executing LLM-generated untrusted code.
//!
//! Honest documentation is the safety story here. See
//! `docs/SANDBOX-POSTURE.md` for the threat model.

use std::time::Duration;

use async_trait::async_trait;
use bellows_core::{BellowsError, DirEntry, ExecOpts, ExecResult, Result, Sandbox};
use tokio::{io::AsyncWriteExt, process::Command};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Subprocess sandbox rooted at a configurable workspace directory.
#[derive(Debug, Clone)]
pub struct LocalSandbox {
    workspace: std::path::PathBuf,
    name: String,
}

impl LocalSandbox {
    /// Build a sandbox rooted at `workspace`. The directory must exist.
    #[must_use]
    pub fn new(workspace: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            name: "local".to_string(),
        }
    }

    /// Resolve `path` relative to the sandbox workspace.
    fn resolve(&self, path: &str) -> std::path::PathBuf {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace.join(p)
        }
    }
}

#[async_trait]
impl Sandbox for LocalSandbox {
    fn name(&self) -> &str {
        &self.name
    }

    async fn exec(&self, cmd: &str, opts: ExecOpts) -> Result<ExecResult> {
        let timeout_ms = opts.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let cwd = opts
            .cwd
            .as_deref()
            .map_or_else(|| self.workspace.clone(), |p| self.resolve(p));

        let started = std::time::Instant::now();

        let mut child = {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd);
            c.current_dir(&cwd);
            c.env_clear();
            for (k, v) in &opts.env {
                c.env(k, v);
            }
            // Restore minimal sane env if caller passed nothing.
            if opts.env.is_empty() {
                c.env("PATH", std::env::var("PATH").unwrap_or_default());
            }
            c.stdin(std::process::Stdio::piped());
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
            c.spawn().map_err(BellowsError::from)?
        };

        if let Some(stdin_payload) = opts.stdin {
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(stdin_payload.as_bytes())
                    .await
                    .map_err(BellowsError::from)?;
            }
        }

        let output = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => Err(BellowsError::from(e)),
            Err(_) => {
                return Ok(ExecResult {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("[bellows] killed: timeout after {timeout_ms}ms"),
                    killed: true,
                    duration_ms: timeout_ms,
                });
            }
        }?;

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(ExecResult {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            killed: false,
            duration_ms,
        })
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>> {
        tokio::fs::read(self.resolve(path))
            .await
            .map_err(BellowsError::from)
    }

    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let resolved = self.resolve(path);
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(BellowsError::from)?;
        }
        tokio::fs::write(resolved, bytes).await.map_err(BellowsError::from)
    }

    async fn list(&self, path: &str) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(self.resolve(path))
            .await
            .map_err(BellowsError::from)?;
        while let Some(entry) = rd.next_entry().await.map_err(BellowsError::from)? {
            let meta = entry.metadata().await.map_err(BellowsError::from)?;
            out.push(DirEntry {
                path: entry.path().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { None } else { Some(meta.len()) },
            });
        }
        Ok(out)
    }
}
