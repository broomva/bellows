//! `bellows-server` — Axum HTTP server wrapping a [`Workflow`].
//!
//! A `bellows build` artifact is a single binary that calls
//! [`serve`] with one or more workflows. The server listens on the default
//! Bellows port `3548` (mnemonic) and exposes:
//!
//! - `GET  /`                        — phone-friendly UI for testing
//! - `GET  /healthz`                 — liveness probe
//! - `GET  /v1/agents`               — list mounted workflows
//! - `POST /v1/agents/{name}`        — invoke a workflow with a JSON body (buffered)
//! - `POST /v1/agents/{name}/stream` — invoke a workflow and stream
//!   events as Server-Sent Events. Each line is one
//!   `data: <json>\n\n` per the contract at
//!   `/tmp/bellows-stream-contract.md`. Stream terminates with `data: [DONE]`.

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use bellows_core::{
    BellowsError, Hook, Result as BellowsResult, StreamEvent, StreamSink, Workflow,
};
use bellows_runtime::Engine;
use bellows_session::MemoryStore;
use futures::{Stream, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

/// Default Bellows server port.
pub const DEFAULT_PORT: u16 = 3548;

const INDEX_HTML: &str = include_str!("ui.html");
const DEFAULT_EXAMPLE_INPUT: &str = "{}";

/// Serve a single workflow on `0.0.0.0:DEFAULT_PORT` until SIGTERM/SIGINT.
///
/// Equivalent to `Server::new(workflow).run().await`.
pub async fn serve<W: Workflow + 'static>(workflow: W) -> std::io::Result<()> {
    Server::new(workflow).run().await
}

/// Configurable server wrapping one workflow.
pub struct Server<W: Workflow> {
    engine: Engine<W>,
    addr: SocketAddr,
    workflow_name: String,
    example_input: String,
}

impl<W: Workflow + 'static> Server<W> {
    /// Build a server with default address `0.0.0.0:DEFAULT_PORT` and an
    /// in-memory session store.
    ///
    /// Honors the standard `PORT` environment variable when set (this is
    /// what Railway, Fly, Render, Heroku, and friends inject into the
    /// runtime). When `PORT` is unset or unparseable, falls back to
    /// [`DEFAULT_PORT`].
    #[must_use]
    pub fn new(workflow: W) -> Self {
        let port = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        let store = Arc::new(MemoryStore::new());
        let workflow_name = workflow.name().to_string();
        Self {
            engine: Engine::new(workflow, store),
            addr: SocketAddr::from(([0, 0, 0, 0], port)),
            workflow_name,
            example_input: DEFAULT_EXAMPLE_INPUT.to_string(),
        }
    }

    /// Override the bind address.
    #[must_use]
    pub const fn bind(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// Set the example JSON input pre-filled in the web UI's request
    /// body box. Improves the phone-test UX — without this, the form
    /// shows `{}` and users have to know the workflow's input shape.
    #[must_use]
    pub fn with_example_input(mut self, json: impl Into<String>) -> Self {
        self.example_input = json.into();
        self
    }

    /// Register a lifecycle hook on the underlying engine. Hooks fire
    /// in registration order at every event (workflow start/end, step
    /// start/end, pre/post inference, pre/post tool use).
    #[must_use]
    pub fn with_hook(mut self, hook: Arc<dyn Hook>) -> Self {
        self.engine = self.engine.with_hook(hook);
        self
    }

    /// Run the server until shutdown signal.
    pub async fn run(self) -> std::io::Result<()> {
        let ui_state = Arc::new(UiState {
            agent_name: self.workflow_name.clone(),
            example_input: self.example_input.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        });
        let engine = Arc::new(self.engine);

        let agent_router = Router::new()
            .route("/v1/agents/{name}", post(invoke::<W>))
            .route("/v1/agents/{name}/stream", post(invoke_stream::<W>))
            .with_state(engine);

        let app = Router::new()
            .route("/", get(index))
            .route("/healthz", get(healthz))
            .route("/v1/agents", get(list_agents))
            .with_state(ui_state)
            .merge(agent_router);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        tracing::info!(
            addr = %self.addr,
            agent = %self.workflow_name,
            "bellows server listening — open http://{} for the web UI",
            self.addr
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
    }
}

#[derive(Clone)]
struct UiState {
    agent_name: String,
    example_input: String,
    version: String,
}

async fn index(State(state): State<Arc<UiState>>) -> Html<String> {
    let html = INDEX_HTML
        .replace("{{AGENT_NAME}}", &state.agent_name)
        .replace(
            "{{EXAMPLE_INPUT}}",
            &escape_for_textarea(&state.example_input),
        )
        .replace("{{VERSION}}", &state.version);
    Html(html)
}

async fn list_agents(State(state): State<Arc<UiState>>) -> Json<serde_json::Value> {
    Json(json!({
        "agents": [
            {
                "name": state.agent_name,
                "endpoint": format!("/v1/agents/{}", state.agent_name),
            }
        ]
    }))
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "bellows" }))
}

async fn invoke<W: Workflow + 'static>(
    State(engine): State<Arc<Engine<W>>>,
    AxumPath(_name): AxumPath<String>,
    Json(input): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let typed = match serde_json::from_value::<W::Input>(input) {
        Ok(v) => v,
        Err(e) => return Json(json!({ "error": format!("invalid input: {e}") })),
    };
    match engine.run(typed).await {
        Ok(out) => match serde_json::to_value(out) {
            Ok(v) => Json(v),
            Err(e) => Json(json!({ "error": format!("output serialization failed: {e}") })),
        },
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

// ── Streaming endpoint ───────────────────────────────────────────────────────

/// Channel-backed `StreamSink` that pushes events into a `tokio::sync::mpsc`.
/// The HTTP handler holds the receiver and feeds it into the SSE response.
///
/// `try_send` is bounded — when the consumer (the HTTP client) is slow,
/// the model stream applies backpressure rather than buffering
/// unboundedly. This is the right default for chat: dropped tokens are
/// worse than a brief pause.
struct ChannelSink {
    tx: mpsc::Sender<StreamEvent>,
}

#[async_trait]
impl StreamSink for ChannelSink {
    async fn emit(&self, event: StreamEvent) -> BellowsResult<()> {
        self.tx
            .send(event)
            .await
            .map_err(|_| BellowsError::Other("client disconnected: sse channel closed".to_string()))
    }
}

/// SSE handler for `POST /v1/agents/{name}/stream`.
///
/// Spawns a task that drives `Engine::run_streaming`, pumping
/// `StreamEvent`s into a channel. The response is an `Sse<...>` whose
/// stream maps each `StreamEvent` to one `Event::default().data(...)`
/// line, terminated with `data: [DONE]`.
///
/// Cancellation: when the client closes the connection, axum drops the
/// response stream, which drops the channel receiver. The next
/// `tx.send()` from the run task will fail, surfacing as
/// `BellowsError::Other("client disconnected")` and unwinding through
/// `run_streaming` — including cancelling the in-flight upstream
/// Anthropic call (because dropping its `Stream` in the
/// `run_inference_streaming` loop closes the underlying reqwest
/// response).
async fn invoke_stream<W: Workflow + 'static>(
    State(engine): State<Arc<Engine<W>>>,
    AxumPath(_name): AxumPath<String>,
    Json(input): Json<serde_json::Value>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    // 64-event buffer is generous — the only producer is the model
    // pump, the only consumer is the SSE flush loop. Anything larger
    // would let backpressure latency grow unboundedly.
    let (tx, rx) = mpsc::channel::<StreamEvent>(64);

    let typed = serde_json::from_value::<W::Input>(input);
    let engine_clone = engine.clone();

    tokio::spawn(async move {
        match typed {
            Ok(input) => {
                let sink: Arc<dyn StreamSink> = Arc::new(ChannelSink { tx: tx.clone() });
                if let Err(err) = engine_clone.run_streaming(input, sink).await {
                    // run_streaming already emitted Error before
                    // returning, but if the channel was closed mid-flight
                    // (client disconnect) the emit would have failed
                    // silently. Best-effort log.
                    tracing::debug!(error = %err, "run_streaming returned with error");
                }
            }
            Err(e) => {
                // Decode failed before we could even start — emit one
                // Error event then drop the sender so the SSE stream
                // terminates with [DONE].
                let _ = tx
                    .send(StreamEvent::Error {
                        message: format!("invalid input: {e}"),
                    })
                    .await;
            }
        }
    });

    let receiver_stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    });

    // Each StreamEvent → one SSE line. The terminal sentinel `[DONE]`
    // is emitted by chaining a final once-stream after the receiver
    // drains.
    let body = receiver_stream
        .map(stream_event_to_sse)
        .chain(futures::stream::once(async {
            Ok::<_, Infallible>(Event::default().data("[DONE]"))
        }));

    Sse::new(body).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Encode a single [`StreamEvent`] into the SSE wire shape declared in
/// `/tmp/bellows-stream-contract.md`. The mapping is intentionally
/// flat (one `data:` line per event, no `event:` field) so the Next.js
/// chat route can parse with a trivial line splitter.
#[allow(clippy::needless_pass_by_value)]
fn stream_event_to_sse(ev: StreamEvent) -> std::result::Result<Event, Infallible> {
    let payload = match ev {
        StreamEvent::SessionStart {
            session_id,
            provider,
            model,
        } => json!({
            "type":       "session_start",
            "session_id": session_id,
            "provider":   provider,
            "model":      model,
        }),
        StreamEvent::TurnStart { turn } => json!({
            "type": "turn_start",
            "turn": turn,
        }),
        StreamEvent::TextDelta { turn, delta } => json!({
            "type":  "text_delta",
            "turn":  turn,
            "delta": delta,
        }),
        StreamEvent::ToolUseStart {
            turn,
            id,
            name,
            label,
        } => json!({
            "type":  "tool_use_start",
            "turn":  turn,
            "id":    id,
            "name":  name,
            "label": label,
        }),
        StreamEvent::ToolUseEnd {
            turn,
            id,
            name,
            ok,
            denied,
            error,
        } => {
            let mut obj = json!({
                "type":   "tool_use_end",
                "turn":   turn,
                "id":     id,
                "name":   name,
                "ok":     ok,
                "denied": denied,
            });
            if let (Some(err), Some(map)) = (error, obj.as_object_mut()) {
                map.insert("error".to_string(), serde_json::Value::String(err));
            }
            obj
        }
        StreamEvent::Done {
            turns,
            stop_reason,
            session_id,
        } => json!({
            "type":        "done",
            "turns":       turns,
            "stop_reason": stop_reason,
            // Contract specifies `tools` here, but the streaming layer
            // already emitted per-call tool_use_start/end events, so
            // sending a redundant final summary would double-render in
            // the chat UI. We send an empty array for forward-compat
            // with consumers that still inspect this field.
            "tools":       [],
            "session_id":  session_id,
        }),
        StreamEvent::Error { message } => json!({
            "type":    "error",
            "message": message,
        }),
        // `#[non_exhaustive]` future-proofing: any new variants land
        // as a generic `error` until an explicit projection ships.
        _ => json!({
            "type":    "error",
            "message": "internal: unhandled stream event variant",
        }),
    };
    let line = serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"sse encode failed\"}".to_string());
    Ok(Event::default().data(line))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("bellows server shutting down");
}

/// Escape JSON for safe embedding inside a `<textarea>`. We can't HTML-encode
/// the entire string (would break JSON parsing in JS), but `<` and `&` are the
/// two chars that can prematurely terminate or open tags inside a textarea.
fn escape_for_textarea(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}
