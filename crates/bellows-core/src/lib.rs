//! # bellows-core
//!
//! Canonical kernel contract for the Bellows agent-harness framework.
//!
//! This crate defines the types and traits that every Bellows agent and
//! every Bellows runtime depends on. It contains **no logic** and has
//! **no implementation dependencies** — just `serde`, `async-trait`,
//! `thiserror`, `ulid`, and `tracing`.
//!
//! ## Mental model
//!
//! A Bellows agent is a [`Workflow`]: a deterministic Rust value that
//! orchestrates one or more autonomous [`Step`]s. Inside each `Step`,
//! the framework runs the model loop (model → tool calls → observations →
//! repeat) until the model emits a final answer. The deterministic outer
//! orchestration is what makes Bellows agents replayable and testable.
//!
//! ```text
//!   Workflow::execute               (deterministic — your code)
//!     └─ ctx.step(Step1).await       (autonomous — model + tools loop)
//!         └─ tool calls inside step  (sandboxed)
//!     └─ ctx.step(Step2).await       (autonomous)
//!     └─ ctx.subagent(Other).await   (spawn isolated workflow)
//! ```

#![doc(html_root_url = "https://docs.rs/bellows-core/0.1.0-pre")]
#![allow(clippy::module_inception)]

pub mod error;
pub mod message;
pub mod model;
pub mod role;
pub mod sandbox;
pub mod session;
pub mod skill;
pub mod step;
pub mod tool;
pub mod workflow;

pub use error::{BellowsError, Result};
pub use message::{Message, MsgRole, ToolCall, ToolResult};
pub use model::{ModelProvider, ModelRequest, ModelResponse, ModelStream, ModelUsage, StopReason};
pub use role::{Role, RoleScope};
pub use sandbox::{DirEntry, ExecOpts, ExecResult, Sandbox};
pub use session::{Session, SessionId, SessionStore};
pub use skill::{Skill, SkillSet};
pub use step::{DEFAULT_INFERENCE_MAX_TURNS, InferenceRequest, Step, StepCtx};
pub use tool::{Tool, ToolRegistry, ToolSchema};
pub use workflow::Workflow;
