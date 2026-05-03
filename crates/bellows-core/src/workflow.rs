//! `Workflow` — the user-defined agent.
//!
//! A `Workflow` is what library users implement. It declares:
//! - the input/output types (`Input`, `Output`),
//! - the agent's identity (`name`, `role`),
//! - the static surface available to it (`skills`, `tools`, `sandbox`, `model`),
//! - the deterministic orchestration (`execute`).
//!
//! The runtime's job is to instantiate the dependencies, feed `execute` a
//! [`StepCtx`], and persist the resulting [`Session`].

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ModelProvider, Result, Role, Sandbox, SkillSet, StepCtx, Tool, skill::EmptySkillSet};

/// User-implemented agent contract.
#[async_trait]
pub trait Workflow: Send + Sync + 'static {
    /// Input shape. Must round-trip through JSON for HTTP / CLI invocation.
    type Input: for<'de> Deserialize<'de> + Send;
    /// Output shape. Must serialize to JSON.
    type Output: Serialize + Send;

    /// Stable workflow name (used as the HTTP route under `/v1/agents/{name}`).
    fn name(&self) -> &str;

    /// Workflow-default role. Lowest precedence; overridden by session and
    /// call roles via [`Role::merge`](crate::Role::merge). Default is empty.
    fn role(&self) -> Role {
        Role::default()
    }

    /// Skills the workflow may consult. Default is empty.
    fn skills(&self) -> &dyn SkillSet {
        &EmptySkillSet
    }

    /// Tools available to the inner model loop. Default is empty —
    /// declare BashTool / FsTool / McpTool here as needed.
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    /// Sandbox connector for the run.
    fn sandbox(&self) -> Arc<dyn Sandbox>;

    /// Model provider for the run.
    fn model(&self) -> Arc<dyn ModelProvider>;

    /// Deterministic orchestration of the agent.
    ///
    /// This body is your code. Call `ctx.step(...).await` to delegate to an
    /// autonomous step. Call `ctx.subagent(...).await` to spawn a child
    /// workflow with a fresh isolated session.
    async fn execute(&self, ctx: &mut StepCtx<'_>, input: Self::Input) -> Result<Self::Output>;
}
