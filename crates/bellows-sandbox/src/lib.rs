//! `bellows-sandbox` — re-exports the [`Sandbox`] trait from `bellows-core`
//! and provides a "virtual" in-process default suitable for fast tests.
//!
//! Real isolation (subprocess, Docker, namespaces, remote) lives in sibling
//! crates: `bellows-sandbox-local`, `bellows-sandbox-docker`,
//! `bellows-sandbox-namespaces`, `bellows-sandbox-e2b`.

use async_trait::async_trait;
use bellows_core::{BellowsError, DirEntry, ExecOpts, ExecResult, Result, Sandbox};

pub use bellows_core::Sandbox as SandboxTrait;

/// In-process "virtual" sandbox.
///
/// Performs **no** isolation; `exec` returns an empty result and `read`/
/// `write`/`list` operate against the host filesystem with no jail. Honest
/// about its posture — never the right choice for untrusted code.
///
/// Useful for tests and for examples that don't shell out at all.
#[derive(Debug, Clone, Default)]
pub struct VirtualSandbox;

#[async_trait]
impl Sandbox for VirtualSandbox {
    fn name(&self) -> &'static str {
        "virtual"
    }

    async fn exec(&self, _cmd: &str, _opts: ExecOpts) -> Result<ExecResult> {
        Err(BellowsError::Sandbox(
            "VirtualSandbox does not execute commands — use bellows-sandbox-local".to_string(),
        ))
    }

    async fn read(&self, path: &str) -> Result<Vec<u8>> {
        tokio::fs::read(path).await.map_err(BellowsError::from)
    }

    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(BellowsError::from)?;
        }
        tokio::fs::write(path, bytes)
            .await
            .map_err(BellowsError::from)
    }

    async fn list(&self, path: &str) -> Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(path)
            .await
            .map_err(BellowsError::from)?;
        while let Some(entry) = rd.next_entry().await.map_err(BellowsError::from)? {
            let meta = entry.metadata().await.map_err(BellowsError::from)?;
            out.push(DirEntry {
                path: entry.path().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() {
                    None
                } else {
                    Some(meta.len())
                },
            });
        }
        Ok(out)
    }
}
