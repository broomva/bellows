//! `bellows-session` — pluggable session storage.
//!
//! Ships a [`MemoryStore`] today. SQLite and Postgres backends are scaffolded
//! behind feature flags in v0.2 (`sqlite`, `postgres`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{BellowsError, Result, Session, SessionId, SessionStore};
use tokio::sync::RwLock;

/// In-memory session store. Useful for tests, `bellows run` one-shots, and
/// development. Not durable — restart loses all sessions.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<RwLock<HashMap<String, Session>>>,
}

impl MemoryStore {
    /// Construct a fresh empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load(&self, id: &SessionId) -> Result<Option<Session>> {
        Ok(self.inner.read().await.get(&id.0).cloned())
    }

    async fn save(&self, session: &Session) -> Result<()> {
        self.inner
            .write()
            .await
            .insert(session.id.0.clone(), session.clone());
        Ok(())
    }

    async fn delete(&self, id: &SessionId) -> Result<()> {
        self.inner.write().await.remove(&id.0);
        Ok(())
    }
}

/// Convert a serde_json error into a `BellowsError::Session` consistently —
/// useful for backends that need to (de)serialize through JSON.
#[must_use]
pub fn json_session_err(e: serde_json::Error) -> BellowsError {
    BellowsError::Session(format!("serialization: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use bellows_core::Message;

    #[tokio::test]
    async fn memory_store_round_trips_a_session() {
        let store = MemoryStore::new();
        let mut sess = Session::new();
        sess.push(Message::user("hello"));
        store.save(&sess).await.unwrap();
        let loaded = store.load(&sess.id).await.unwrap().unwrap();
        assert_eq!(loaded.history.len(), 1);
        store.delete(&sess.id).await.unwrap();
        assert!(store.load(&sess.id).await.unwrap().is_none());
    }
}
