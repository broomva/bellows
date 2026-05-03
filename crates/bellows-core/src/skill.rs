//! `Skill` and `SkillSet` — Markdown-defined reusable agent capabilities.
//!
//! A `Skill` is a `(frontmatter, body)` pair parsed from a Markdown file with
//! YAML frontmatter — the same shape used by Anthropic Skills, Flue skills,
//! and Broomva's `skills/` directory. Skills are loaded once at agent
//! construction and queried by name during execution.
//!
//! Skill bodies are *prompt content*: they are pasted into the system or user
//! turn (the `Workflow` decides where) when the skill is invoked. Frontmatter
//! is structured metadata (input/output schemas, tags, version, etc.).
//!
//! Skill loading lives in the `bellows-skill` crate; this contract defines the
//! shape only.

use serde::{Deserialize, Serialize};

/// One Markdown skill: parsed frontmatter + raw body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Stable name (kebab-case, derived from filename or explicit frontmatter).
    pub name: String,
    /// Parsed YAML frontmatter as a generic JSON value. Workflows that need
    /// typed access can `serde_json::from_value` into their own struct.
    pub frontmatter: serde_json::Value,
    /// Raw Markdown body (everything after the second `---`).
    pub body: String,
}

/// A bundle of skills, looked up by name. The contract is a trait so that
/// implementations can choose between embedded (`include_dir!`),
/// runtime-loaded, or merged sources.
pub trait SkillSet: Send + Sync {
    /// Look up a skill by name.
    fn get(&self, name: &str) -> Option<&Skill>;

    /// Iterate all skills in stable order.
    fn all(&self) -> &[Skill];

    /// Convenience: number of skills in the set.
    fn len(&self) -> usize {
        self.all().len()
    }

    /// Convenience: whether the set is empty.
    fn is_empty(&self) -> bool {
        self.all().is_empty()
    }
}

/// Empty `SkillSet` used as the default when a workflow declares no skills.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptySkillSet;

impl SkillSet for EmptySkillSet {
    fn get(&self, _name: &str) -> Option<&Skill> {
        None
    }

    fn all(&self) -> &[Skill] {
        &[]
    }
}
