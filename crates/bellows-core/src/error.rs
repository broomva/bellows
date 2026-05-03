//! Error types for the Bellows kernel contract.
//!
//! `BellowsError` is the single error type returned by every async trait
//! method in the contract. Implementations are encouraged to convert their
//! domain errors into `BellowsError` variants at the trait boundary so
//! downstream code (the runtime) only ever matches against a single type.

use std::result::Result as StdResult;

use thiserror::Error;

/// Result alias used by every fallible API in the kernel contract.
pub type Result<T> = StdResult<T, BellowsError>;

/// Canonical error type returned across the kernel contract boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BellowsError {
    /// The model provider returned a transport, parse, or protocol error.
    #[error("model provider error: {0}")]
    Model(String),

    /// The sandbox could not execute or filesystem operation failed.
    #[error("sandbox error: {0}")]
    Sandbox(String),

    /// A tool invocation failed (tool returned an error or arguments rejected).
    #[error("tool `{name}` failed: {reason}")]
    Tool {
        /// Logical name of the tool that failed.
        name: String,
        /// Human-readable reason supplied by the tool implementation.
        reason: String,
    },

    /// Skill loading or parsing failed.
    #[error("skill error: {0}")]
    Skill(String),

    /// Session storage error (load/save).
    #[error("session error: {0}")]
    Session(String),

    /// A required configuration value was missing or invalid.
    #[error("configuration error: {0}")]
    Config(String),

    /// The workflow itself produced a domain error.
    #[error("workflow error: {0}")]
    Workflow(String),

    /// I/O error not covered by another variant (e.g. sandbox file read).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Serialization / deserialization failed somewhere on the boundary.
    #[error("serialization error: {0}")]
    Serde(String),

    /// Catch-all for cases that genuinely do not fit another variant.
    /// Use sparingly — prefer typed variants where possible.
    #[error("{0}")]
    Other(String),
}

impl From<serde_json::Error> for BellowsError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e.to_string())
    }
}
