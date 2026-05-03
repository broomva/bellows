//! `bellows-server` — Axum HTTP server wrapping a [`Workflow`].
//!
//! A `bellows build` artifact is a single binary that calls
//! [`serve`] with one or more workflows. The server listens on the default
//! Bellows port `3548` (mnemonic) and exposes:
//!
//! - `GET  /`                 — phone-friendly UI for testing
//! - `GET  /healthz`          — liveness probe
//! - `GET  /v1/agents`        — list mounted workflows
//! - `POST /v1/agents/{name}` — invoke a workflow with a JSON body
//!
//! Streaming endpoints (`/v1/agents/:name/stream`) land in v0.2 once the
//! `ModelStreamEvent` mapping is locked.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    response::Html,
    routing::{get, post},
};
use bellows_core::{Hook, Workflow};
use bellows_runtime::Engine;
use bellows_session::MemoryStore;
use serde_json::json;

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
    #[must_use]
    pub fn new(workflow: W) -> Self {
        let store = Arc::new(MemoryStore::new());
        let workflow_name = workflow.name().to_string();
        Self {
            engine: Engine::new(workflow, store),
            addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
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
