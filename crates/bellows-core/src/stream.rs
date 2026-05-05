//! Streaming event taxonomy for the autonomous loop.
//!
//! `StepCtx::run_inference_streaming` and `Engine::run_streaming` walk
//! the agent loop the same way the buffered variants do, but every
//! observable transition is fanned out to a [`StreamSink`] in real
//! time. The sink is the bridge between the runtime's typed view and
//! whatever wire format the consumer wants — a `tokio::sync::mpsc`
//! channel for the HTTP server, a JSONL file for replay tools, an
//! in-memory accumulator for tests.
//!
//! ## Event order (informally)
//!
//! ```text
//!   session_start
//!     turn_start { turn: 0 }
//!       text_delta*
//!       tool_use_start
//!         tool_use_end
//!       text_delta*
//!     turn_start { turn: 1 }                  ← only if loop continues
//!     ...
//!   done { stop_reason, turns, ... }
//! ```
//!
//! `error` may be emitted from any point. After `done` or an `error`,
//! the sink will not be called again.
//!
//! The contract is documented in detail at
//! `/tmp/bellows-stream-contract.md` (v0.2-pre-streaming).

use async_trait::async_trait;

use crate::{Result, StopReason, ToolCall, ToolResult};

/// Strongly typed streaming event surface.
///
/// Mirrors the SSE contract one for one — every variant maps to a
/// single `data: {...}` line on the wire — but stays in Rust types so
/// the runtime never has to round-trip through JSON internally.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamEvent {
    /// First event of a streaming run. Emitted by the engine before
    /// `Workflow::execute` begins.
    SessionStart {
        /// ULID of the session being executed.
        session_id: String,
        /// Provider id (`"anthropic"`, `"mock"`).
        provider: String,
        /// Model id (e.g. `"claude-sonnet-4-5"`).
        model: String,
    },
    /// One iteration of the autonomous loop (one model call) has begun.
    /// `turn` is 0-indexed.
    TurnStart {
        /// 0-indexed turn number within the current `run_inference_streaming` call.
        turn: u32,
    },
    /// A chunk of model-emitted text. Concatenating all `delta` values
    /// for a turn reconstructs the assistant's prose.
    TextDelta {
        /// Turn this delta belongs to.
        turn: u32,
        /// Raw text chunk — pass through to UIs verbatim.
        delta: String,
    },
    /// The model has decided to invoke a tool. Always paired with a
    /// later `ToolUseEnd` carrying the same `id`.
    ToolUseStart {
        /// Turn this tool call belongs to.
        turn: u32,
        /// Provider-supplied call id (e.g. `"toolu_01XYZ"`).
        id: String,
        /// Tool name as it appears in `Tool::schema().name`.
        name: String,
        /// Short human-readable label derived from the call's arguments
        /// (file path, shell command, …). Empty when no specialised
        /// formatter is available.
        label: String,
    },
    /// A tool call finished. `ok` is false when the tool errored, the
    /// hook denied, or the tool returned `is_error=true`. `denied` is
    /// true only when an `on_pre_tool_use` hook returned `Deny`.
    ToolUseEnd {
        /// Turn this tool call belongs to.
        turn: u32,
        /// Same id as the matching `ToolUseStart`.
        id: String,
        /// Tool name (echoed from `ToolUseStart` to keep handlers stateless).
        name: String,
        /// `false` if the tool failed, was denied, or returned `is_error=true`.
        ok: bool,
        /// `true` when an `on_pre_tool_use` hook returned `Deny`.
        denied: bool,
        /// Optional human-readable error message; only set when `ok=false`.
        error: Option<String>,
    },
    /// Terminal event for a successful run. Emitted exactly once after
    /// `Workflow::execute` returns.
    Done {
        /// Total number of assistant turns across the workflow.
        turns: u32,
        /// Final stop reason from the last model turn.
        stop_reason: StopReason,
        /// Session id (echoed from `SessionStart` for clients that
        /// register on `Done`).
        session_id: String,
    },
    /// Unrecoverable error. After this, the sink is not called again
    /// and the stream terminates.
    Error {
        /// Human-readable error message.
        message: String,
    },
}

/// One-way streaming consumer. Implementations push events into
/// whatever transport (mpsc channel, log file, accumulator) the
/// caller prefers.
///
/// The runtime calls `emit` from inside the tokio task that drives the
/// agent loop, so implementations must be `Send + Sync`. They should
/// be cheap (a single `try_send` / `write_all` per call); slow sinks
/// will impose backpressure on the model stream.
#[async_trait]
pub trait StreamSink: Send + Sync {
    /// Emit one streaming event.
    ///
    /// Returning `Err` aborts the streaming loop with `BellowsError`;
    /// implementations that want to be cancellable on consumer
    /// disconnect should map `mpsc::error::SendError` (or equivalent)
    /// into a `BellowsError::Other("client disconnected")` and return
    /// it here.
    async fn emit(&self, event: StreamEvent) -> Result<()>;
}

/// In-memory `StreamSink` that accumulates events into a `Vec` for tests.
///
/// Cheap to clone — internally an `Arc<Mutex<Vec<_>>>`. Test bodies
/// drive the agent against a `BufferSink::new()`, then call
/// `into_inner()` (or `snapshot()`) once the run finishes.
#[derive(Debug, Default, Clone)]
pub struct BufferSink {
    inner: std::sync::Arc<std::sync::Mutex<Vec<StreamEvent>>>,
}

impl BufferSink {
    /// Empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the events so far (does not drain).
    #[must_use]
    pub fn snapshot(&self) -> Vec<StreamEvent> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Drain the events and consume the sink.
    #[must_use]
    pub fn into_inner(self) -> Vec<StreamEvent> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

#[async_trait]
impl StreamSink for BufferSink {
    async fn emit(&self, event: StreamEvent) -> Result<()> {
        if let Ok(mut g) = self.inner.lock() {
            g.push(event);
        }
        Ok(())
    }
}

/// Build the short human-readable label that accompanies a
/// `ToolUseStart` event. Specialised for the built-in tools shipped in
/// `bellows-tool`; falls back to compact JSON otherwise.
///
/// Lives in `bellows-core` so the streaming layer can format labels
/// without depending on `bellows-tool` (which depends on us). The chat
/// UI surfaces this directly.
#[must_use]
pub fn format_tool_label(name: &str, args: &serde_json::Value) -> String {
    let s = |key: &str| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "fs_list" | "fs_read" => {
            let p = s("path");
            if p.is_empty() {
                "(no path)".to_string()
            } else {
                p
            }
        }
        "fs_write" => {
            let p = s("path");
            let bytes = args
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map_or(0, str::len);
            if p.is_empty() {
                format!("(no path, {bytes}B)")
            } else {
                format!("{p} ({bytes}B)")
            }
        }
        "bash" => {
            let cmd = s("cmd");
            if cmd.len() > 80 {
                format!("{}…", &cmd[..80])
            } else {
                cmd
            }
        }
        _ => serde_json::to_string(args)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect(),
    }
}

/// Convenience: build a `ToolUseEnd` from a finished call + result +
/// the `denied` flag. Centralised so server and runtime emit the
/// identical shape.
#[must_use]
pub fn tool_end_event(
    turn: u32,
    call: &ToolCall,
    result: &ToolResult,
    denied: bool,
) -> StreamEvent {
    let ok = !result.is_error;
    let error = if ok {
        None
    } else {
        result
            .output
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| Some("tool returned is_error=true".to_string()))
    };
    StreamEvent::ToolUseEnd {
        turn,
        id: call.id.clone(),
        name: call.name.clone(),
        ok,
        denied,
        error,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn label_fs_read_returns_path() {
        let label = format_tool_label("fs_read", &json!({"path": "/etc/hosts"}));
        assert_eq!(label, "/etc/hosts");
    }

    #[test]
    fn label_fs_write_includes_byte_count() {
        let label = format_tool_label("fs_write", &json!({"path": "/tmp/x", "content": "hello"}));
        assert_eq!(label, "/tmp/x (5B)");
    }

    #[test]
    fn label_bash_truncates_long_commands() {
        let cmd = "echo ".repeat(40);
        let label = format_tool_label("bash", &json!({"cmd": cmd}));
        assert!(label.ends_with('…'));
    }

    #[test]
    fn tool_end_event_marks_error_when_result_is_error() {
        let call = ToolCall {
            id: "id1".into(),
            name: "fs_read".into(),
            arguments: json!({}),
        };
        let result = ToolResult {
            call_id: "id1".into(),
            output: json!({"error": "permission denied"}),
            is_error: true,
        };
        match tool_end_event(0, &call, &result, false) {
            StreamEvent::ToolUseEnd {
                ok, denied, error, ..
            } => {
                assert!(!ok);
                assert!(!denied);
                assert_eq!(error.as_deref(), Some("permission denied"));
            }
            other => panic!("expected ToolUseEnd, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn buffer_sink_collects_events_in_order() {
        let sink = BufferSink::new();
        sink.emit(StreamEvent::TurnStart { turn: 0 }).await.unwrap();
        sink.emit(StreamEvent::TextDelta {
            turn: 0,
            delta: "hi".into(),
        })
        .await
        .unwrap();
        let snap = sink.snapshot();
        assert_eq!(snap.len(), 2);
    }
}
