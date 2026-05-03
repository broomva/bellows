//! `Hook` — lifecycle callbacks for the autonomous loop.
//!
//! Hooks are the Bellows analogue of Claude Code's `.claude/settings.json`
//! event pipeline (`PreToolUse`, `PostToolUse`, `SessionStart`, `Stop`,
//! `Notification`, `PreCompact`). They give framework users a place to
//! plug in audit logging, approval gates, budget caps, knowledge-graph
//! capture, and arbitrary side effects without modifying the runtime.
//!
//! ## Design
//!
//! - In-process Rust trait callbacks — fast, type-safe, share memory with
//!   the agent.
//! - Each hook event method has a default `Continue` implementation, so
//!   implementations only override the events they care about.
//! - Outcomes are typed: `HookOutcome` for observation-or-veto;
//!   `ToolHookOutcome` for tools where a hook may also *stub* a synthetic
//!   result; `InferenceHookOutcome` for model calls where a hook may
//!   stub a synthetic assistant message.
//! - Hooks are registered on the [`Engine`](crate::Workflow) builder and
//!   walked in registration order at every lifecycle point.
//!
//! ## Hook events
//!
//! | Event | When | Outcome |
//! |---|---|---|
//! | `on_workflow_start` | engine begins running a workflow | `HookOutcome` |
//! | `on_workflow_end` | engine finishes (success or error) | `HookOutcome` |
//! | `on_step_start` | a `Step` is about to run | `HookOutcome` |
//! | `on_step_end` | a `Step` finished | `HookOutcome` |
//! | `on_pre_inference` | about to call the model | `InferenceHookOutcome` |
//! | `on_post_inference` | model responded | `HookOutcome` |
//! | `on_pre_tool_use` | about to invoke a tool | `ToolHookOutcome` |
//! | `on_post_tool_use` | tool finished | `HookOutcome` |
//!
//! ## Comparison to Claude Code hooks
//!
//! Claude Code shells out to external commands per
//! `.claude/settings.json`. Bellows hooks are in-process trait
//! implementations. Equivalent expressive power, vastly faster, share the
//! agent's trust posture (so use them on code you trust). A future
//! `bellows-hooks-shellout` crate may add the external-command path.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{Message, ModelRequest, ModelResponse, Result, Session, ToolCall, ToolResult};

/// Context handed to every hook invocation.
///
/// Cheap to construct (just borrows). Carries enough handle to log,
/// correlate, or read session state — but not enough to mutate session
/// state directly. Hooks that need to mutate session state should do so
/// via the explicit outcome variants (`Stub`).
pub struct HookCtx<'a> {
    /// The workflow's stable name (`Workflow::name`).
    pub workflow_name: &'a str,
    /// The session being executed.
    pub session: &'a Session,
    /// The active tracing span for this lifecycle moment. Hooks can
    /// emit child spans/events using this as a parent.
    pub trace: &'a tracing::Span,
}

/// Generic hook outcome — observe or veto.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// Continue with the action the hook observed.
    Continue,
    /// Abort the action with a human-readable reason. The runtime
    /// surfaces this as `BellowsError::Workflow`.
    Deny(String),
}

/// Outcome for tool-use hooks.
#[derive(Debug, Clone)]
pub enum ToolHookOutcome {
    /// Run the tool normally.
    Continue,
    /// Skip the tool. The runtime synthesizes an error
    /// `tool_result { is_error: true }` carrying `reason`, which lets
    /// the model adapt.
    Deny(String),
    /// Skip the tool and supply this synthetic JSON value as its
    /// result. The model will see a normal `tool_result`. Useful for
    /// caching, mocking in tests, and approval-then-replace flows.
    Stub(serde_json::Value),
}

/// Outcome for pre-inference hooks.
#[derive(Debug, Clone)]
pub enum InferenceHookOutcome {
    /// Call the model normally.
    Continue,
    /// Abort the inference loop with a reason. The runtime returns
    /// `BellowsError::Workflow`.
    Deny(String),
    /// Skip the model call and supply this synthetic assistant message.
    /// Useful for replay tests, caching, and offline-mode degradation.
    Stub(Message),
}

/// One hook implementation. Override only the events you care about.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Stable name for logs and error messages.
    fn name(&self) -> &str;

    /// The engine is about to start `Workflow::execute`.
    async fn on_workflow_start(&self, _ctx: &HookCtx<'_>) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// The engine just finished `Workflow::execute`. `succeeded` is
    /// `false` if the workflow returned an error.
    async fn on_workflow_end(&self, _ctx: &HookCtx<'_>, _succeeded: bool) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// A `Step` is about to run.
    async fn on_step_start(&self, _ctx: &HookCtx<'_>, _step_name: &str) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// A `Step` just finished. `succeeded` is `false` if the step
    /// returned an error.
    async fn on_step_end(
        &self,
        _ctx: &HookCtx<'_>,
        _step_name: &str,
        _succeeded: bool,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// About to call the model. Hooks may mutate the request, deny, or
    /// short-circuit with a stub message.
    async fn on_pre_inference(
        &self,
        _ctx: &HookCtx<'_>,
        _request: &mut ModelRequest,
    ) -> Result<InferenceHookOutcome> {
        Ok(InferenceHookOutcome::Continue)
    }

    /// Model responded. Hooks observe; they can deny but not modify.
    async fn on_post_inference(
        &self,
        _ctx: &HookCtx<'_>,
        _response: &ModelResponse,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// About to invoke a tool. Hooks may mutate `call.arguments`, deny
    /// the call, or short-circuit with a stub result.
    async fn on_pre_tool_use(
        &self,
        _ctx: &HookCtx<'_>,
        _call: &mut ToolCall,
    ) -> Result<ToolHookOutcome> {
        Ok(ToolHookOutcome::Continue)
    }

    /// Tool finished. Hooks observe (and may mutate the result, e.g. to
    /// redact secrets) or deny — denying replaces the result with an
    /// error.
    async fn on_post_tool_use(
        &self,
        _ctx: &HookCtx<'_>,
        _call: &ToolCall,
        _result: &mut ToolResult,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }
}

/// Ordered collection of hooks. Walked in registration order at every
/// lifecycle point.
#[derive(Default, Clone)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookRegistry {
    /// Empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook. Hooks fire in the order they were registered.
    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    /// Builder-style helper.
    #[must_use]
    pub fn with(mut self, hook: Arc<dyn Hook>) -> Self {
        self.register(hook);
        self
    }

    /// Hooks in registration order.
    #[must_use]
    pub fn list(&self) -> &[Arc<dyn Hook>] {
        &self.hooks
    }

    /// True iff no hooks are registered. The runtime can skip the
    /// registry entirely on the empty path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl std::fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.hooks.iter().map(|h| h.name()).collect();
        f.debug_struct("HookRegistry")
            .field("hooks", &names)
            .finish()
    }
}

// ── Reference implementation: TracingHook ────────────────────────────────────

/// Reference hook that emits a `tracing::info!` event at every
/// lifecycle point. Useful as a starting point and for observability
/// when no other hooks are registered.
///
/// Stays in `bellows-core` because it has no I/O — it just routes
/// through `tracing`, which is already a kernel-contract dependency.
/// File-output / JSONL / OTLP-bridge hooks live in a future
/// `bellows-hooks` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingHook;

#[async_trait]
impl Hook for TracingHook {
    fn name(&self) -> &str {
        "tracing"
    }

    async fn on_workflow_start(&self, ctx: &HookCtx<'_>) -> Result<HookOutcome> {
        tracing::info!(parent: ctx.trace, workflow = ctx.workflow_name, session = %ctx.session.id, "workflow.start");
        Ok(HookOutcome::Continue)
    }

    async fn on_workflow_end(&self, ctx: &HookCtx<'_>, succeeded: bool) -> Result<HookOutcome> {
        tracing::info!(parent: ctx.trace, workflow = ctx.workflow_name, session = %ctx.session.id, succeeded, "workflow.end");
        Ok(HookOutcome::Continue)
    }

    async fn on_step_start(&self, ctx: &HookCtx<'_>, step_name: &str) -> Result<HookOutcome> {
        tracing::info!(parent: ctx.trace, step = step_name, session = %ctx.session.id, "step.start");
        Ok(HookOutcome::Continue)
    }

    async fn on_step_end(
        &self,
        ctx: &HookCtx<'_>,
        step_name: &str,
        succeeded: bool,
    ) -> Result<HookOutcome> {
        tracing::info!(parent: ctx.trace, step = step_name, session = %ctx.session.id, succeeded, "step.end");
        Ok(HookOutcome::Continue)
    }

    async fn on_pre_inference(
        &self,
        ctx: &HookCtx<'_>,
        request: &mut ModelRequest,
    ) -> Result<InferenceHookOutcome> {
        tracing::info!(parent: ctx.trace, model = %request.model, history = request.messages.len(), tools = request.tools.len(), "inference.pre");
        Ok(InferenceHookOutcome::Continue)
    }

    async fn on_post_inference(
        &self,
        ctx: &HookCtx<'_>,
        response: &ModelResponse,
    ) -> Result<HookOutcome> {
        let usage_in = response.usage.as_ref().map_or(0, |u| u.input_tokens);
        let usage_out = response.usage.as_ref().map_or(0, |u| u.output_tokens);
        tracing::info!(parent: ctx.trace, stop = ?response.stop_reason, tool_calls = response.message.tool_calls.len(), input_tokens = usage_in, output_tokens = usage_out, "inference.post");
        Ok(HookOutcome::Continue)
    }

    async fn on_pre_tool_use(
        &self,
        ctx: &HookCtx<'_>,
        call: &mut ToolCall,
    ) -> Result<ToolHookOutcome> {
        tracing::info!(parent: ctx.trace, tool = %call.name, id = %call.id, "tool.pre");
        Ok(ToolHookOutcome::Continue)
    }

    async fn on_post_tool_use(
        &self,
        ctx: &HookCtx<'_>,
        call: &ToolCall,
        result: &mut ToolResult,
    ) -> Result<HookOutcome> {
        tracing::info!(parent: ctx.trace, tool = %call.name, id = %call.id, is_error = result.is_error, "tool.post");
        Ok(HookOutcome::Continue)
    }
}

// ── Reference implementation: AllowDenyHook ──────────────────────────────────

/// Pattern-match approval/deny hook for tools.
///
/// Allow-list mode: only tools whose name appears in `allow` may run;
/// everything else is denied with a fixed reason.
///
/// Deny-list mode: tools in `deny` are denied; everything else runs.
///
/// Use exactly one of `allow` or `deny` per instance. The hook does not
/// modify arguments — it only inspects names. For richer policies (e.g.
/// regex on file paths in `fs_write` arguments) implement your own
/// `Hook` instead.
#[derive(Debug, Clone)]
pub struct AllowDenyHook {
    name: String,
    /// Allow-list of tool names. If `Some(_)`, only listed tools run.
    pub allow: Option<Vec<String>>,
    /// Deny-list of tool names. If `Some(_)`, listed tools are denied.
    pub deny: Option<Vec<String>>,
    /// Reason returned to the model when a call is denied.
    pub reason: String,
}

impl AllowDenyHook {
    /// Build an allow-list hook.
    #[must_use]
    pub fn allow_only(allow: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            name: "allow-deny".to_string(),
            allow: Some(allow.into_iter().map(Into::into).collect()),
            deny: None,
            reason: "tool not on allow-list".to_string(),
        }
    }

    /// Build a deny-list hook.
    #[must_use]
    pub fn deny_list(deny: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            name: "allow-deny".to_string(),
            allow: None,
            deny: Some(deny.into_iter().map(Into::into).collect()),
            reason: "tool on deny-list".to_string(),
        }
    }

    /// Builder: override the deny reason.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

#[async_trait]
impl Hook for AllowDenyHook {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    async fn on_pre_tool_use(
        &self,
        _ctx: &HookCtx<'_>,
        call: &mut ToolCall,
    ) -> Result<ToolHookOutcome> {
        if let Some(allow) = &self.allow {
            if !allow.iter().any(|n| n == &call.name) {
                return Ok(ToolHookOutcome::Deny(self.reason.clone()));
            }
        }
        if let Some(deny) = &self.deny {
            if deny.iter().any(|n| n == &call.name) {
                return Ok(ToolHookOutcome::Deny(self.reason.clone()));
            }
        }
        Ok(ToolHookOutcome::Continue)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dummy_session() -> Session {
        Session::new()
    }

    fn dummy_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: name.to_string(),
            arguments: json!({}),
        }
    }

    #[tokio::test]
    async fn allow_list_passes_listed_tools() {
        let h = AllowDenyHook::allow_only(["bash", "fs_read"]);
        let session = dummy_session();
        let span = tracing::Span::none();
        let ctx = HookCtx {
            workflow_name: "test",
            session: &session,
            trace: &span,
        };
        let mut call = dummy_call("fs_read");
        let outcome = h.on_pre_tool_use(&ctx, &mut call).await.unwrap();
        assert!(matches!(outcome, ToolHookOutcome::Continue));
    }

    #[tokio::test]
    async fn allow_list_denies_unlisted_tools() {
        let h = AllowDenyHook::allow_only(["bash"]);
        let session = dummy_session();
        let span = tracing::Span::none();
        let ctx = HookCtx {
            workflow_name: "test",
            session: &session,
            trace: &span,
        };
        let mut call = dummy_call("fs_write");
        let outcome = h.on_pre_tool_use(&ctx, &mut call).await.unwrap();
        assert!(matches!(outcome, ToolHookOutcome::Deny(_)));
    }

    #[tokio::test]
    async fn deny_list_blocks_listed_tools() {
        let h = AllowDenyHook::deny_list(["fs_write", "bash"]);
        let session = dummy_session();
        let span = tracing::Span::none();
        let ctx = HookCtx {
            workflow_name: "test",
            session: &session,
            trace: &span,
        };
        let mut call = dummy_call("bash");
        let outcome = h.on_pre_tool_use(&ctx, &mut call).await.unwrap();
        assert!(matches!(outcome, ToolHookOutcome::Deny(_)));
    }

    #[test]
    fn registry_preserves_registration_order() {
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(TracingHook));
        reg.register(Arc::new(AllowDenyHook::deny_list(["x"])));
        let names: Vec<&str> = reg.list().iter().map(|h| h.name()).collect();
        assert_eq!(names, vec!["tracing", "allow-deny"]);
    }
}
