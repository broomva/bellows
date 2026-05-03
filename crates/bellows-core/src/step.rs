//! `Step` and `StepCtx` — the autonomous-step boundary.
//!
//! A `Step` is the unit of *autonomous* work inside a deterministic
//! [`Workflow`](crate::Workflow). When a workflow calls
//! [`StepCtx::step`](StepCtx::step) on a step, the runtime opens an inner
//! loop:
//!
//! ```text
//!   1. Build ModelRequest from session + role + tools
//!   2. Call ModelProvider::stream
//!   3. If StopReason::ToolUse: invoke tools, append results to session, goto 1
//!   4. Else: append final message to session, return Output
//! ```
//!
//! This loop is the only place in Bellows where non-determinism lives.
//! Everything outside `Step::run` is deterministic and replayable.
//!
//! `StepCtx` is what the runtime hands the step at call time. It exposes
//! the session, model, tools, sandbox, skills, and a tracing span — and
//! offers `subagent` for spawning isolated child workflows.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{ModelProvider, Result, Sandbox, Session, SkillSet, ToolRegistry};

/// One autonomous step inside a workflow.
///
/// Implementations describe the contract for their step (Input/Output types,
/// instructions added to the model request, tool restrictions if any) and
/// implement `run` to execute it. The runtime supplies the loop machinery —
/// most `run` bodies are short.
#[async_trait]
pub trait Step: Send + Sync {
    /// Input fed into this step.
    type Input: Send;
    /// Output produced by this step.
    type Output: Send;

    /// Stable name for this step (used in tracing spans and audit logs).
    fn name(&self) -> &str;

    /// Execute the step.
    async fn run(&self, ctx: &mut StepCtx<'_>, input: Self::Input) -> Result<Self::Output>;
}

/// Context passed into [`Step::run`]. The runtime owns these; the step borrows.
pub struct StepCtx<'a> {
    /// Mutable handle to the session — appending messages here makes them
    /// visible to subsequent steps and persisted by the `SessionStore`.
    pub session: &'a mut Session,
    /// Model provider for this run.
    pub model: Arc<dyn ModelProvider>,
    /// Tool registry available to the inner model loop.
    pub tools: Arc<dyn ToolRegistry>,
    /// Sandbox the runtime hands to tools that need it.
    pub sandbox: Arc<dyn Sandbox>,
    /// Skills declared by the workflow.
    pub skills: &'a dyn SkillSet,
    /// Tracing span the runtime opened around this step. Children should be
    /// created via `tracing::info_span!(parent: &ctx.trace, ...)`.
    pub trace: tracing::Span,
}

// Note: `step` and `subagent` invocations are dispatched by the runtime.
// `bellows-core` declares the contract; `bellows-runtime` provides the
// extension methods that close the loop. See `bellows_runtime::ctx_ext`.
impl StepCtx<'_> {}
