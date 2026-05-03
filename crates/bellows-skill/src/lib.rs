//! `bellows-skill` — Markdown skill loader.
//!
//! Parses the same `frontmatter + body` shape used by Anthropic Skills,
//! Flue, and Broomva's `skills/` directory: a YAML block delimited by
//! `---\n` markers at the top, then Markdown content.
//!
//! Two loading paths:
//!
//! - **Embedded** via `include_dir!` — used in production builds. Skills
//!   are compiled into the binary; no runtime IO.
//! - **Runtime** via [`load_dir`] — used in `bellows dev` for hot-reload.
//!
//! Both produce a [`SkillBundle`] which implements
//! [`bellows_core::SkillSet`].

#![allow(clippy::missing_errors_doc)]

use bellows_core::{Skill, SkillSet};
use thiserror::Error;

/// Errors specific to skill loading.
#[derive(Debug, Error)]
pub enum SkillError {
    /// The frontmatter delimiters were missing or malformed.
    #[error("malformed frontmatter in `{file}`: {reason}")]
    BadFrontmatter {
        /// Path that failed to parse.
        file: String,
        /// Specific reason.
        reason: String,
    },

    /// YAML deserialization failed.
    #[error("yaml error in `{file}`: {source}")]
    Yaml {
        /// Path that failed.
        file: String,
        /// Inner YAML error.
        #[source]
        source: serde_yaml_ng::Error,
    },

    /// I/O error while reading skills from disk.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A concrete bundle of skills implementing [`SkillSet`].
#[derive(Debug, Clone, Default)]
pub struct SkillBundle {
    skills: Vec<Skill>,
}

impl SkillBundle {
    /// Build a bundle from an explicit list. Used by [`load_dir`] and the
    /// `embed!` macro path.
    #[must_use]
    pub const fn from_vec(skills: Vec<Skill>) -> Self {
        Self { skills }
    }
}

impl SkillSet for SkillBundle {
    fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    fn all(&self) -> &[Skill] {
        &self.skills
    }
}

/// Parse a single Markdown skill file's contents into a `Skill`.
///
/// `name` is supplied by the caller (typically derived from the filename).
/// Frontmatter is parsed as YAML into a `serde_json::Value` so workflows can
/// either use it generically or `serde_json::from_value` into a typed shape.
pub fn parse_skill(name: &str, source: &str) -> Result<Skill, SkillError> {
    let (frontmatter_str, body) = split_frontmatter(name, source)?;

    let frontmatter: serde_json::Value =
        serde_yaml_ng::from_str(frontmatter_str).map_err(|source| SkillError::Yaml {
            file: name.to_string(),
            source,
        })?;

    Ok(Skill {
        name: name.to_string(),
        frontmatter,
        body: body.to_string(),
    })
}

/// Split a Markdown source into `(frontmatter_yaml, body)`.
/// Frontmatter is required to be present and to be the very first thing in
/// the file — `---\n...\n---\n`. Anything else is rejected.
fn split_frontmatter<'a>(name: &str, source: &'a str) -> Result<(&'a str, &'a str), SkillError> {
    let rest = source
        .strip_prefix("---\n")
        .ok_or_else(|| SkillError::BadFrontmatter {
            file: name.to_string(),
            reason: "missing leading `---\\n` marker".to_string(),
        })?;
    let end_idx = rest
        .find("\n---\n")
        .or_else(|| rest.find("\n---"))
        .ok_or_else(|| SkillError::BadFrontmatter {
            file: name.to_string(),
            reason: "missing closing `---` marker".to_string(),
        })?;
    let frontmatter = &rest[..end_idx];
    let after = &rest[end_idx..];
    let body = after
        .strip_prefix("\n---\n")
        .or_else(|| after.strip_prefix("\n---"))
        .unwrap_or("");
    Ok((frontmatter, body.trim_start_matches('\n')))
}

/// Load all `*.md` files in `dir` (non-recursive) as skills.
/// Skill name is derived from the filename without the `.md` extension.
pub fn load_dir(dir: impl AsRef<std::path::Path>) -> Result<SkillBundle, SkillError> {
    let mut skills = Vec::new();
    for entry in std::fs::read_dir(dir.as_ref())? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let source = std::fs::read_to_string(&path)?;
        skills.push(parse_skill(&name, &source)?);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(SkillBundle::from_vec(skills))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: triage\nversion: 1\n---\n\n# Triage\n\nClassify the issue.\n";

    #[test]
    fn parse_minimal_skill() {
        let s = parse_skill("triage", SAMPLE).unwrap();
        assert_eq!(s.name, "triage");
        assert_eq!(s.frontmatter["name"], "triage");
        assert_eq!(s.frontmatter["version"], 1);
        assert!(s.body.starts_with("# Triage"));
    }

    #[test]
    fn missing_frontmatter_marker_is_rejected() {
        let r = parse_skill("bad", "just markdown");
        assert!(matches!(r, Err(SkillError::BadFrontmatter { .. })));
    }
}
