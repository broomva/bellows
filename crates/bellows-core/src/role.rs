//! `Role` — the system-prompt overlay primitive.
//!
//! A `Role` is a *non-persistent* identity/instructions overlay applied to the
//! request sent to a [`ModelProvider`](crate::ModelProvider). Roles are the
//! Bellows equivalent of Flue's roles; they are applied at request-build time
//! and **never inserted into [`Session`](crate::Session) history**. This keeps
//! conversation history clean and replayable across role swaps.
//!
//! ## Precedence
//!
//! `Role::merge` enforces a single, audited precedence:
//!
//! ```text
//!   call > session > agent
//! ```
//!
//! A call-scoped role wins over a session-scoped role, which wins over the
//! agent's default role. This is the only place precedence is computed —
//! callers must not re-implement this rule.

use serde::{Deserialize, Serialize};

/// Where in the runtime hierarchy this `Role` was supplied.
///
/// Lower-numbered scopes win during [`Role::merge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleScope {
    /// Per-invocation override (highest precedence).
    Call,
    /// Per-session override (middle precedence).
    Session,
    /// Workflow default (lowest precedence).
    Agent,
}

impl Default for RoleScope {
    fn default() -> Self {
        Self::Agent
    }
}

/// A `Role` overlays identity and instruction text onto a [`ModelRequest`].
///
/// Construct via `Role::default()` then mutate, or via builder-style
/// `with_*` methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Role {
    /// One-line identity string (e.g. "Issue triage agent").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Instruction lines, prepended to the system prompt in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<String>,
    /// Where in the hierarchy this role was supplied.
    #[serde(default)]
    pub scope: RoleScope,
}

impl Role {
    /// Builder: set the identity line.
    #[must_use]
    pub fn with_identity<S: Into<String>>(mut self, identity: S) -> Self {
        self.identity = Some(identity.into());
        self
    }

    /// Builder: append an instruction line.
    #[must_use]
    pub fn with_instruction<S: Into<String>>(mut self, instruction: S) -> Self {
        self.instructions.push(instruction.into());
        self
    }

    /// Builder: set scope.
    #[must_use]
    pub const fn with_scope(mut self, scope: RoleScope) -> Self {
        self.scope = scope;
        self
    }

    /// Merge three roles using the canonical precedence (`call > session > agent`).
    ///
    /// The result borrows non-empty fields from the highest-precedence role
    /// that supplied them. Instructions concatenate in agent → session → call
    /// order (lowest precedence first, so the strongest instructions appear
    /// last and have the most recency-bias on the model).
    #[must_use]
    pub fn merge(agent: &Self, session: Option<&Self>, call: Option<&Self>) -> Self {
        let identity = call
            .and_then(|r| r.identity.clone())
            .or_else(|| session.and_then(|r| r.identity.clone()))
            .or_else(|| agent.identity.clone());

        let mut instructions = Vec::new();
        instructions.extend(agent.instructions.iter().cloned());
        if let Some(s) = session {
            instructions.extend(s.instructions.iter().cloned());
        }
        if let Some(c) = call {
            instructions.extend(c.instructions.iter().cloned());
        }

        Self {
            identity,
            instructions,
            scope: call.map_or(
                session.map_or(RoleScope::Agent, |_| RoleScope::Session),
                |_| RoleScope::Call,
            ),
        }
    }

    /// Render this role as a single system-prompt string. Returns `None` when
    /// the role contributes nothing (no identity, no instructions).
    #[must_use]
    pub fn render(&self) -> Option<String> {
        if self.identity.is_none() && self.instructions.is_empty() {
            return None;
        }
        let mut out = String::new();
        if let Some(id) = &self.identity {
            out.push_str(id);
            out.push_str("\n\n");
        }
        for line in &self.instructions {
            out.push_str(line);
            out.push('\n');
        }
        Some(out.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_call_overrides_session_overrides_agent() {
        let agent = Role::default()
            .with_identity("agent-id")
            .with_instruction("agent-1");
        let session = Role::default()
            .with_identity("session-id")
            .with_instruction("session-1");
        let call = Role::default()
            .with_identity("call-id")
            .with_instruction("call-1");

        let merged = Role::merge(&agent, Some(&session), Some(&call));
        assert_eq!(merged.identity.as_deref(), Some("call-id"));
        assert_eq!(merged.scope, RoleScope::Call);
        // Instructions concatenate lowest-precedence first.
        assert_eq!(
            merged.instructions,
            vec![
                "agent-1".to_string(),
                "session-1".to_string(),
                "call-1".to_string(),
            ],
        );
    }

    #[test]
    fn merge_with_no_overlays_returns_agent_identity() {
        let agent = Role::default().with_identity("agent-only");
        let merged = Role::merge(&agent, None, None);
        assert_eq!(merged.identity.as_deref(), Some("agent-only"));
        assert_eq!(merged.scope, RoleScope::Agent);
    }

    #[test]
    fn render_yields_none_for_empty_role() {
        assert!(Role::default().render().is_none());
    }
}
