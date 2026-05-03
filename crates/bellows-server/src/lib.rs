//! `bellows-server` — Axum HTTP server wrapping a [`Workflow`].
//!
//! A `bellows build` artifact is a single binary that calls
//! [`serve`] with one or more workflows. The server listens on the default
//! Bellows port `3548` (mnemonic) and exposes:
//!
//! - `GET  /healthz`          — liveness probe
//! - `POST /v1/agents/:name`  — invoke a workflow with a JSON body
//!
//! Streaming endpoints (`/v1/agents/:name/stream`) land in v0.2 once the
//! `ModelStreamEvent` mapping is locked.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use bellows_core::Workflow;
use bellows_runtime::Engine;
use bellows_session::MemoryStore;
use serde_json::json;

/// Default Bellows server port.
pub const DEFAULT_PORT: u16 = 3548;

/// Serve a single workflow on `0.0.0.0:DEFAULT_PORT` until SIGTERM/SIGINT.
///
/// Equivalent to `Server::new(workflow).run().await`.
pub async fn serve<W: Workflow + 'static>(workflow: W) -> std::io::Result<()> {
    Server::new(workflow).run().await
}

/// Configurable server wrapping one workflow.
pub struct Server<W: Workflow> {
    engine: Arc<Engine<W>>,
    addr: SocketAddr,
}

impl<W: Workflow + 'static> Server<W> {
    /// Build a server with default address `0.0.0.0:DEFAULT_PORT` and an
    /// in-memory session store.
    #[must_use]
    pub fn new(workflow: W) -> Self {
        let store = Arc::new(MemoryStore::new());
        Self {
            engine: Arc::new(Engine::new(workflow, store)),
            addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
        }
    }

    /// Override the bind address.
    #[must_use]
    pub const fn bind(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// Run the server until shutdown signal.
    pub async fn run(self) -> std::io::Result<()> {
        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/agents/:name", post(invoke::<W>))
            .with_state(self.engine.clone());

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        tracing::info!(addr = %self.addr, "bellows server listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
    }
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
