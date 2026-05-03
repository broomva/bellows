//! `Session` — persistent conversation history for one workflow invocation.
//!
//! A `Session` carries:
//! - a stable id (ULID, lexicographically sortable),
//! - the full ordered [`Message`] history,
//! - the session-scoped [`Role`] (if any),
//! - opaque metadata for runtime use.
//!
//! Persistence is pluggable via the [`SessionStore`] trait. The kernel
//! contract ships the trait definition; implementations live in
//! `bellows-session` (in-memory + SQLite + Postgres feature-gated).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{Message, Result, Role};

/// Stable, sortable session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    /// Generate a fresh ULID-based session id.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One agent invocation worth of state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Session identifier.
    pub id: SessionId,
    /// Conversation history in chronological order.
    #[serde(default)]
    pub history: Vec<Message>,
    /// Session-scoped role overlay. Merged at request-build time using
    /// [`Role::merge`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Free-form metadata bag for runtime annotations (trace ids, parent
    /// session for subagents, model used, etc.).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl Session {
    /// Create a fresh session with a new ULID id and no history.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: SessionId::new(),
            history: Vec::new(),
            role: None,
            meta: serde_json::Map::new(),
        }
    }

    /// Append a message to history. Returns the new length.
    pub fn push(&mut self, msg: Message) -> usize {
        self.history.push(msg);
        self.history.len()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistent storage for [`Session`] values.
///
/// Implementations are expected to be cheap to clone and `Send + Sync` —
/// runtimes share a single store across many concurrent sessions.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Load a session by id. Returns `Ok(None)` if not found.
    async fn load(&self, id: &SessionId) -> Result<Option<Session>>;

    /// Save (insert or update) a session.
    async fn save(&self, session: &Session) -> Result<()>;

    /// Delete a session by id. Idempotent — deleting a missing id succeeds.
    async fn delete(&self, id: &SessionId) -> Result<()>;
}
