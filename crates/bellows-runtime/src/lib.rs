//! `bellows-runtime` — the workflow engine.
//!
//! Drives a [`Workflow`] end-to-end:
//!
//! 1. Load or create a [`Session`].
//! 2. Build a [`StepCtx`] with model + tools + sandbox + skills + hooks.
//! 3. Fire `on_workflow_start` hooks.
//! 4. Call `Workflow::execute(ctx, input)`.
//! 5. Fire `on_workflow_end` hooks.
//! 6. Persist the resulting session.
//!
//! The autonomous step + tool-dispatch loop lives in `bellows-core`'s
//! [`StepCtx::run_inference`]. The runtime owns construction and
//! workflow-level lifecycle hooks.

use std::sync::Arc;

use bellows_core::{
    BellowsError, HookCtx, HookOutcome, HookRegistry, ModelProvider, Result, Sandbox, Session,
    SessionStore, SkillSet, StepCtx, StopReason, StreamEvent, StreamSink, ToolRegistry, Workflow,
};
use bellows_tool::SimpleRegistry;
use tracing::{Span, info_span};

/// One configured runtime instance for a specific workflow.
pub struct Engine<W: Workflow> {
    workflow: Arc<W>,
    store: Arc<dyn SessionStore>,
    hooks: Arc<HookRegistry>,
}

impl<W: Workflow> Engine<W> {
    /// Construct an engine with a custom session store. Starts with no
    /// hooks registered — add them via [`Engine::with_hook`].
    #[must_use]
    pub fn new(workflow: W, store: Arc<dyn SessionStore>) -> Self {
        Self {
            workflow: Arc::new(workflow),
            store,
            hooks: Arc::new(HookRegistry::new()),
        }
    }

    /// Register a hook. Hooks fire in the order they are added at every
    /// lifecycle point exposed by the framework (workflow start/end,
    /// step start/end, pre/post inference, pre/post tool use).
    #[must_use]
    pub fn with_hook(mut self, hook: Arc<dyn bellows_core::Hook>) -> Self {
        let mut reg = (*self.hooks).clone();
        reg.register(hook);
        self.hooks = Arc::new(reg);
        self
    }

    /// Replace the entire hook registry. Convenient when callers
    /// already assembled a `HookRegistry` (e.g. shared across engines).
    #[must_use]
    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = Arc::new(hooks);
        self
    }

    /// Hooks currently registered on this engine.
    #[must_use]
    pub fn hooks(&self) -> &HookRegistry {
        &self.hooks
    }

    /// Run the workflow once. Creates a fresh session, fires hooks,
    /// invokes the workflow, persists the session.
    pub async fn run(&self, input: W::Input) -> Result<W::Output> {
        let mut session = Session::new();
        let span = info_span!("workflow.run", name = self.workflow.name(), session = %session.id);

        let model = self.workflow.model();
        let sandbox = self.workflow.sandbox();
        let tools_vec = self.workflow.tools();
        let mut registry = SimpleRegistry::new();
        for t in tools_vec {
            registry.register(t);
        }
        let tools: Arc<dyn ToolRegistry> = Arc::new(registry);

        // on_workflow_start hooks. A deny here aborts before invoke and
        // skips the on_workflow_end hooks (workflow never started).
        if !self.hooks.is_empty() {
            let hook_ctx = HookCtx {
                workflow_name: self.workflow.name(),
                session: &session,
                trace: &span,
            };
            for hook in self.hooks.list() {
                if let HookOutcome::Deny(reason) = hook.on_workflow_start(&hook_ctx).await? {
                    return Err(BellowsError::Workflow(format!(
                        "hook `{}` denied workflow start: {reason}",
                        hook.name(),
                    )));
                }
            }
        }

        let result = self
            .invoke(&mut session, model, tools, sandbox, &span, input)
            .await;

        // on_workflow_end hooks (best-effort; their errors do not
        // override the workflow's own result).
        if !self.hooks.is_empty() {
            let succeeded = result.is_ok();
            let hook_ctx = HookCtx {
                workflow_name: self.workflow.name(),
                session: &session,
                trace: &span,
            };
            for hook in self.hooks.list() {
                match hook.on_workflow_end(&hook_ctx, succeeded).await {
                    Ok(HookOutcome::Continue) => {}
                    Ok(HookOutcome::Deny(reason)) => {
                        tracing::warn!(
                            hook = hook.name(),
                            workflow = self.workflow.name(),
                            "post-workflow deny ignored: {reason}"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            hook = hook.name(),
                            workflow = self.workflow.name(),
                            error = %err,
                            "post-workflow hook error ignored"
                        );
                    }
                }
            }
        }

        let output = result?;
        self.store.save(&session).await?;
        Ok(output)
    }

    async fn invoke(
        &self,
        session: &mut Session,
        model: Arc<dyn ModelProvider>,
        tools: Arc<dyn ToolRegistry>,
        sandbox: Arc<dyn Sandbox>,
        span: &Span,
        input: W::Input,
    ) -> Result<W::Output> {
        self.invoke_with_sink(session, model, tools, sandbox, span, input, None)
            .await
    }

    /// Same wiring as `invoke`, but threads an optional [`StreamSink`]
    /// down into the [`StepCtx`]. When `Some(_)`, the workflow's
    /// `run_inference` calls auto-route through
    /// `run_inference_streaming` with zero workflow-source churn.
    #[allow(clippy::too_many_arguments)]
    async fn invoke_with_sink(
        &self,
        session: &mut Session,
        model: Arc<dyn ModelProvider>,
        tools: Arc<dyn ToolRegistry>,
        sandbox: Arc<dyn Sandbox>,
        span: &Span,
        input: W::Input,
        sink: Option<Arc<dyn StreamSink>>,
    ) -> Result<W::Output> {
        let skills: &dyn SkillSet = self.workflow.skills();
        let workflow_name = self.workflow.name();
        let mut ctx = StepCtx {
            session,
            model,
            tools,
            sandbox,
            skills,
            hooks: self.hooks.clone(),
            workflow_name,
            trace: span.clone(),
            stream_sink: sink,
        };
        self.workflow.execute(&mut ctx, input).await
    }

    /// Streaming sibling of [`Engine::run`].
    ///
    /// Same wiring as `run` — fresh session, model + sandbox + tools +
    /// hooks built from the workflow — but every observable
    /// transition is fanned out to `sink` in real time:
    ///
    /// 1. `SessionStart` (with `session_id`, provider id, model id) —
    ///    immediately, before any hooks fire.
    /// 2. The workflow's `run_inference` calls automatically use the
    ///    streaming model path (because `StepCtx::stream_sink` is
    ///    populated). They emit `TurnStart`, `TextDelta`, and
    ///    `ToolUseStart`/`End` events as the model produces them.
    /// 3. After `Workflow::execute` returns, `Done` (with `turns`,
    ///    `stop_reason`, and the same `session_id`).
    /// 4. On any error path, `Error` (with the stringified
    ///    `BellowsError`) and the run aborts.
    ///
    /// The `Output` value the workflow returned is intentionally NOT
    /// re-emitted on the wire — the SSE contract handles tool summaries
    /// via `Done`'s shape. Callers that need the typed `Output`
    /// concurrently with the stream can wrap their workflow in one that
    /// pushes via a side channel; for the common chat case the streamed
    /// events are already the full UI payload.
    #[allow(clippy::too_many_lines)]
    pub async fn run_streaming(&self, input: W::Input, sink: Arc<dyn StreamSink>) -> Result<()> {
        let mut session = Session::new();
        let span = info_span!("workflow.run_streaming", name = self.workflow.name(), session = %session.id);

        let model = self.workflow.model();
        let sandbox = self.workflow.sandbox();
        let tools_vec = self.workflow.tools();
        let mut registry = SimpleRegistry::new();
        for t in tools_vec {
            registry.register(t);
        }
        let tools: Arc<dyn ToolRegistry> = Arc::new(registry);

        // Always emit SessionStart first so consumers can latch the id
        // even if the workflow denies in on_workflow_start.
        sink.emit(StreamEvent::SessionStart {
            session_id: session.id.to_string(),
            provider: model.id().to_string(),
            model: String::new(), // model id is per-step, not per-engine; clients can read from TurnStart context
        })
        .await?;

        if !self.hooks.is_empty() {
            let hook_ctx = HookCtx {
                workflow_name: self.workflow.name(),
                session: &session,
                trace: &span,
            };
            for hook in self.hooks.list() {
                if let HookOutcome::Deny(reason) = hook.on_workflow_start(&hook_ctx).await? {
                    let msg = format!("hook `{}` denied workflow start: {reason}", hook.name());
                    sink.emit(StreamEvent::Error {
                        message: msg.clone(),
                    })
                    .await?;
                    return Err(BellowsError::Workflow(msg));
                }
            }
        }

        let result = self
            .invoke_with_sink(
                &mut session,
                model,
                tools,
                sandbox,
                &span,
                input,
                Some(sink.clone()),
            )
            .await;

        if !self.hooks.is_empty() {
            let succeeded = result.is_ok();
            let hook_ctx = HookCtx {
                workflow_name: self.workflow.name(),
                session: &session,
                trace: &span,
            };
            for hook in self.hooks.list() {
                match hook.on_workflow_end(&hook_ctx, succeeded).await {
                    Ok(HookOutcome::Continue) => {}
                    Ok(HookOutcome::Deny(reason)) => {
                        tracing::warn!(
                            hook = hook.name(),
                            workflow = self.workflow.name(),
                            "post-workflow deny ignored: {reason}"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            hook = hook.name(),
                            workflow = self.workflow.name(),
                            error = %err,
                            "post-workflow hook error ignored"
                        );
                    }
                }
            }
        }

        match result {
            Ok(_) => {
                let turns = u32::try_from(
                    session
                        .history
                        .iter()
                        .filter(|m| matches!(m.role, bellows_core::MsgRole::Assistant))
                        .count(),
                )
                .unwrap_or(u32::MAX);
                // Stop reason: derive from the final assistant message —
                // we don't have a typed channel back from
                // `run_inference_streaming`, so default to EndTurn for
                // successful exits. Workflows that need precise
                // stop_reason reporting can emit it themselves via the
                // hook layer.
                sink.emit(StreamEvent::Done {
                    turns,
                    stop_reason: StopReason::EndTurn,
                    session_id: session.id.to_string(),
                })
                .await?;
                self.store.save(&session).await?;
                Ok(())
            }
            Err(err) => {
                sink.emit(StreamEvent::Error {
                    message: err.to_string(),
                })
                .await?;
                Err(err)
            }
        }
    }
}
