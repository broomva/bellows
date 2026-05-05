//! `Step` and `StepCtx` — the autonomous-step boundary.
//!
//! A [`Step`] is the unit of *autonomous* work inside a deterministic
//! [`Workflow`](crate::Workflow). The autonomous loop — model call →
//! tool dispatch → observation → repeat — runs inside
//! [`StepCtx::run_inference`]. Everything outside that call is
//! deterministic and replayable.
//!
//! Two methods drive most user code:
//!
//! - [`StepCtx::step`] — scope a child step under its own tracing span and
//!   delegate to the step's `run` body.
//! - [`StepCtx::run_inference`] — drive the autonomous model + tool loop
//!   against the configured provider until the model emits a final
//!   `EndTurn`-class stop.
//!
//! Like [`Role::merge`](crate::Role::merge), this logic lives in
//! `bellows-core` because there is exactly one correct protocol
//! implementation; downstream crates configure the *behavior* (which
//! provider, which tools, which sandbox) but not the loop itself.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tracing::Instrument;

use crate::{
    BellowsError, HookCtx, HookOutcome, HookRegistry, InferenceHookOutcome, Message, ModelProvider,
    ModelRequest, ModelResponse, ModelStreamEvent, ModelUsage, MsgRole, Result, Role, Sandbox,
    Session, SkillSet, StopReason, Tool, ToolCall, ToolHookOutcome, ToolRegistry, ToolResult,
    ToolSchema,
    stream::{StreamEvent, StreamSink, format_tool_label, tool_end_event},
};

/// Soft cap on autonomous-loop iterations to prevent runaway agents.
///
/// One iteration = one model call + zero-or-more tool invocations. Real
/// workflows almost always finish in 1–5 iterations; the default cap of 16
/// is generous.
pub const DEFAULT_INFERENCE_MAX_TURNS: u32 = 16;

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
    /// Lifecycle hooks fired by the autonomous loop and by
    /// [`StepCtx::step`]. Walked in registration order at every event.
    pub hooks: Arc<HookRegistry>,
    /// Workflow name carried for hook context. Filled by the runtime
    /// when constructing the `StepCtx`.
    pub workflow_name: &'a str,
    /// Tracing span the runtime opened around this step. Children should be
    /// created via `tracing::info_span!(parent: &ctx.trace, ...)`.
    pub trace: tracing::Span,
    /// Optional streaming sink. When set (by the runtime's `run_streaming`
    /// path), [`StepCtx::run_inference`] auto-routes to the streaming
    /// loop and emits per-delta events; existing workflows pick up real
    /// streaming with zero source changes. `None` = buffered loop.
    pub stream_sink: Option<Arc<dyn StreamSink>>,
}

/// Configuration for a single autonomous-loop invocation.
///
/// Built explicitly so callers see exactly which knobs they're setting.
#[derive(Debug, Clone)]
pub struct InferenceRequest {
    /// Provider-specific model id.
    pub model: String,
    /// Optional role overlay (call-scoped). Merged with session + agent
    /// roles by the caller before passing in. The loop applies this to
    /// every iteration's request, exactly once.
    pub role: Option<Role>,
    /// Optional cap on output tokens per turn. `None` = provider default.
    pub max_tokens: Option<u32>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Stop sequences. Empty = none.
    pub stop: Vec<String>,
    /// Maximum loop iterations. Default: [`DEFAULT_INFERENCE_MAX_TURNS`].
    pub max_turns: u32,
}

impl Default for InferenceRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            role: None,
            max_tokens: None,
            temperature: None,
            stop: Vec::new(),
            max_turns: DEFAULT_INFERENCE_MAX_TURNS,
        }
    }
}

impl InferenceRequest {
    /// Convenience constructor with required model id.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    /// Builder: set the role overlay.
    #[must_use]
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Builder: set max tokens.
    #[must_use]
    pub const fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Builder: set temperature.
    #[must_use]
    pub const fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Builder: set max turns.
    #[must_use]
    pub const fn with_max_turns(mut self, n: u32) -> Self {
        self.max_turns = n;
        self
    }
}

impl StepCtx<'_> {
    /// Build a fresh `HookCtx` borrowing this step's session + workflow
    /// name + tracing span.
    const fn hook_ctx(&self) -> HookCtx<'_> {
        HookCtx {
            workflow_name: self.workflow_name,
            session: self.session,
            trace: &self.trace,
        }
    }

    /// Scope a child [`Step`] under its own tracing span and delegate to
    /// `s.run(self, input)`. Fires `on_step_start` and `on_step_end`
    /// hooks around the call.
    ///
    /// This is the canonical way for a [`Workflow::execute`] body to
    /// orchestrate inner steps. Each call gets its own `step` span so
    /// observability tools can show the workflow's structure.
    pub async fn step<S: Step + ?Sized>(&mut self, s: &S, input: S::Input) -> Result<S::Output> {
        let span = tracing::info_span!(parent: &self.trace, "step", name = s.name());

        // Pre-step hooks
        if !self.hooks.is_empty() {
            let hook_ctx = self.hook_ctx();
            for hook in self.hooks.list() {
                if let HookOutcome::Deny(reason) = hook.on_step_start(&hook_ctx, s.name()).await? {
                    return Err(BellowsError::Workflow(format!(
                        "hook `{}` denied step `{}`: {reason}",
                        hook.name(),
                        s.name(),
                    )));
                }
            }
        }

        let result = async { s.run(self, input).await }.instrument(span).await;
        let succeeded = result.is_ok();

        // Post-step hooks (best-effort: a hook error here is logged but
        // does not override the step's own result)
        if !self.hooks.is_empty() {
            let hook_ctx = self.hook_ctx();
            for hook in self.hooks.list() {
                match hook.on_step_end(&hook_ctx, s.name(), succeeded).await {
                    Ok(HookOutcome::Continue) => {}
                    Ok(HookOutcome::Deny(reason)) => {
                        tracing::warn!(
                            hook = hook.name(),
                            step = s.name(),
                            "post-step deny ignored: {reason}"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(hook = hook.name(), step = s.name(), error = %err, "post-step hook error ignored");
                    }
                }
            }
        }

        result
    }

    /// Drive the autonomous loop: model call → tool dispatch → repeat
    /// until the provider emits a final, non-tool stop reason or
    /// `req.max_turns` is reached.
    ///
    /// Behavior:
    ///
    /// 1. Build a [`ModelRequest`] from `self.session.history` plus the
    ///    schemas of every tool currently in `self.tools`.
    /// 2. Call `self.model.complete`.
    /// 3. Push the assistant message into `self.session.history`.
    /// 4. If the response carries `tool_calls`, invoke each one against
    ///    `self.sandbox` and append the results as a single `MsgRole::Tool`
    ///    message; goto 1.
    /// 5. Otherwise return the final assistant message to the caller.
    ///
    /// Errors:
    /// - Provider transport / decode failures bubble as
    ///   [`BellowsError::Model`].
    /// - A `tool_use` referencing a tool not in the registry returns
    ///   [`BellowsError::Tool`] with `name` set.
    /// - Hitting `req.max_turns` returns [`BellowsError::Workflow`] so the
    ///   workflow body can catch and recover if needed.
    ///
    /// **Streaming mode:** when the runtime constructed this `StepCtx`
    /// via [`Engine::run_streaming`], `self.stream_sink` is `Some(_)` and
    /// this method transparently routes to
    /// [`StepCtx::run_inference_streaming`], so existing workflows pick
    /// up real-time streaming with zero source changes.
    pub async fn run_inference(&mut self, req: &InferenceRequest) -> Result<Message> {
        if let Some(sink) = self.stream_sink.clone() {
            return self.run_inference_streaming(req, sink.as_ref()).await;
        }
        let tool_schemas: Vec<ToolSchema> = self.tools.list().iter().map(|t| t.schema()).collect();

        for turn in 0..req.max_turns {
            let mut model_request = ModelRequest {
                model: req.model.clone(),
                messages: self.session.history.clone(),
                role: req.role.clone(),
                tools: tool_schemas.clone(),
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
            };

            // Pre-inference hooks: may mutate request, deny, or stub.
            if !self.hooks.is_empty() {
                let hook_ctx = self.hook_ctx();
                let mut stub: Option<Message> = None;
                for hook in self.hooks.list() {
                    match hook.on_pre_inference(&hook_ctx, &mut model_request).await? {
                        InferenceHookOutcome::Continue => {}
                        InferenceHookOutcome::Deny(reason) => {
                            return Err(BellowsError::Workflow(format!(
                                "hook `{}` denied inference: {reason}",
                                hook.name()
                            )));
                        }
                        InferenceHookOutcome::Stub(msg) => {
                            stub = Some(msg);
                            break;
                        }
                    }
                }
                if let Some(msg) = stub {
                    self.session.push(msg.clone());
                    return Ok(msg);
                }
            }

            let span = tracing::info_span!(
                parent: &self.trace,
                "inference.turn",
                turn,
                model = %req.model,
            );
            let resp = async { self.model.complete(model_request).await }
                .instrument(span)
                .await?;

            // Post-inference hooks (observation/veto, no mutation).
            if !self.hooks.is_empty() {
                let hook_ctx = self.hook_ctx();
                for hook in self.hooks.list() {
                    if let HookOutcome::Deny(reason) =
                        hook.on_post_inference(&hook_ctx, &resp).await?
                    {
                        return Err(BellowsError::Workflow(format!(
                            "hook `{}` denied response: {reason}",
                            hook.name()
                        )));
                    }
                }
            }

            // Always persist the assistant message — including ones with
            // tool_calls — before dispatching, so the session history is
            // chronologically correct even if a tool fails mid-loop.
            self.session.push(resp.message.clone());

            match resp.stop_reason {
                StopReason::ToolUse => {
                    if resp.message.tool_calls.is_empty() {
                        // Defensive: provider claimed tool_use but emitted
                        // no calls. Treat as terminal to avoid an infinite
                        // loop.
                        return Ok(resp.message);
                    }
                    let results = self.dispatch_tool_calls(&resp.message.tool_calls).await?;
                    self.session.push(Message {
                        role: MsgRole::Tool,
                        content: String::new(),
                        tool_calls: Vec::new(),
                        tool_results: results,
                    });
                }
                StopReason::EndTurn
                | StopReason::MaxTokens
                | StopReason::StopSequence
                | StopReason::Other => {
                    return Ok(resp.message);
                }
            }
        }

        Err(BellowsError::Workflow(format!(
            "run_inference: exceeded max_turns ({}) without a final stop",
            req.max_turns
        )))
    }

    /// Dispatch a batch of tool calls against the active sandbox, with
    /// hooks fired pre- and post- each call.
    ///
    /// Each call passes through `on_pre_tool_use` (which may mutate
    /// arguments, deny the call, or stub a synthetic result) and
    /// `on_post_tool_use` (which may mutate the result or convert it to
    /// an error). Calls are dispatched **sequentially** in v0.1;
    /// parallel dispatch via `FuturesOrdered` lands in v0.4.
    async fn dispatch_tool_calls(&self, calls: &[ToolCall]) -> Result<Vec<ToolResult>> {
        self.dispatch_tool_calls_with_sink(calls, None, 0).await
    }

    /// Variant of `dispatch_tool_calls` that, when `sink` is `Some`,
    /// emits one [`StreamEvent::ToolUseStart`] before invoking each
    /// tool and one [`StreamEvent::ToolUseEnd`] after. Used by
    /// `run_inference_streaming`. The non-streaming path passes
    /// `None` so behavior is byte-identical.
    async fn dispatch_tool_calls_with_sink(
        &self,
        calls: &[ToolCall],
        sink: Option<&dyn StreamSink>,
        turn: u32,
    ) -> Result<Vec<ToolResult>> {
        let mut out = Vec::with_capacity(calls.len());
        for call in calls {
            let mut call = call.clone();

            // Pre-tool-use hooks
            let mut hook_outcome: ToolHookOutcome = ToolHookOutcome::Continue;
            if !self.hooks.is_empty() {
                let hook_ctx = self.hook_ctx();
                for hook in self.hooks.list() {
                    match hook.on_pre_tool_use(&hook_ctx, &mut call).await? {
                        ToolHookOutcome::Continue => {}
                        denied @ ToolHookOutcome::Deny(_) => {
                            hook_outcome = denied;
                            break;
                        }
                        stubbed @ ToolHookOutcome::Stub(_) => {
                            hook_outcome = stubbed;
                            break;
                        }
                    }
                }
            }

            // Whether this call was stopped by a hook deny — needed for the
            // ToolUseEnd event's `denied` flag.
            let denied = matches!(hook_outcome, ToolHookOutcome::Deny(_));

            // Emit ToolUseStart only after hooks have observed (so a
            // stub-replacing hook can mutate `call.arguments` and the
            // `label` we emit reflects what actually got invoked).
            if let Some(s) = sink {
                let label = format_tool_label(&call.name, &call.arguments);
                s.emit(StreamEvent::ToolUseStart {
                    turn,
                    id: call.id.clone(),
                    name: call.name.clone(),
                    label,
                })
                .await?;
            }

            let mut result = match hook_outcome {
                ToolHookOutcome::Deny(reason) => ToolResult {
                    call_id: call.id.clone(),
                    output: serde_json::json!({ "error": reason, "denied_by": "hook" }),
                    is_error: true,
                },
                ToolHookOutcome::Stub(value) => ToolResult {
                    call_id: call.id.clone(),
                    output: value,
                    is_error: false,
                },
                ToolHookOutcome::Continue => {
                    let span = tracing::info_span!(
                        parent: &self.trace,
                        "tool.invoke",
                        name = %call.name,
                        id = %call.id,
                    );
                    let tool: Arc<dyn Tool> =
                        self.tools
                            .get(&call.name)
                            .ok_or_else(|| BellowsError::Tool {
                                name: call.name.clone(),
                                reason: "tool not registered".to_string(),
                            })?;
                    let invocation = async {
                        tool.invoke(call.arguments.clone(), self.sandbox.as_ref())
                            .await
                    }
                    .instrument(span)
                    .await;

                    match invocation {
                        Ok(value) => ToolResult {
                            call_id: call.id.clone(),
                            output: value,
                            is_error: false,
                        },
                        Err(err) => ToolResult {
                            call_id: call.id.clone(),
                            output: serde_json::json!({ "error": err.to_string() }),
                            is_error: true,
                        },
                    }
                }
            };

            // Post-tool-use hooks
            if !self.hooks.is_empty() {
                let hook_ctx = self.hook_ctx();
                for hook in self.hooks.list() {
                    match hook.on_post_tool_use(&hook_ctx, &call, &mut result).await? {
                        HookOutcome::Continue => {}
                        HookOutcome::Deny(reason) => {
                            result.output =
                                serde_json::json!({ "error": reason, "denied_by": "hook" });
                            result.is_error = true;
                        }
                    }
                }
            }

            // Emit ToolUseEnd after post-hooks have settled the final result.
            if let Some(s) = sink {
                s.emit(tool_end_event(turn, &call, &result, denied)).await?;
            }

            out.push(result);
        }
        Ok(out)
    }

    /// Streaming sibling of [`StepCtx::run_inference`].
    ///
    /// Same loop semantics — model call, optional tool dispatch, repeat
    /// until a non-tool stop reason or `req.max_turns` — but every
    /// observable transition is fanned out to `sink` in real time:
    ///
    /// - [`StreamEvent::TurnStart`] before each model call.
    /// - [`StreamEvent::TextDelta`] for each `ModelStreamEvent::TextDelta`.
    /// - [`StreamEvent::ToolUseStart`] / `ToolUseEnd` around each tool
    ///   invocation, identical to the non-streaming dispatch path.
    ///
    /// The function does NOT emit `SessionStart` or `Done` — those are
    /// the engine layer's responsibility (the engine wraps the workflow
    /// invocation, this fn only sees one `run_inference` call).
    ///
    /// Tool-call argument deltas (`ModelStreamEvent::ToolCallDelta`) are
    /// **buffered into the assistant message** rather than fanned out
    /// — the contract treats them as an implementation detail of the
    /// upstream provider. The fully-assembled `arguments` JSON is then
    /// what feeds into `dispatch_tool_calls_with_sink`, so the chat UI
    /// gets a well-formed `ToolUseStart` with a real label.
    ///
    /// Hook lifecycle is preserved: pre-inference hooks see the
    /// outgoing request, post-inference hooks see the **fully
    /// accumulated** assistant message (not per-delta partials), and
    /// per-tool hooks fire identically to the non-streaming path.
    #[allow(clippy::too_many_lines)]
    pub async fn run_inference_streaming(
        &mut self,
        req: &InferenceRequest,
        sink: &dyn StreamSink,
    ) -> Result<Message> {
        let tool_schemas: Vec<ToolSchema> = self.tools.list().iter().map(|t| t.schema()).collect();

        for turn in 0..req.max_turns {
            let mut model_request = ModelRequest {
                model: req.model.clone(),
                messages: self.session.history.clone(),
                role: req.role.clone(),
                tools: tool_schemas.clone(),
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
            };

            // Pre-inference hooks — same shape as the buffered loop.
            if !self.hooks.is_empty() {
                let hook_ctx = self.hook_ctx();
                let mut stub: Option<Message> = None;
                for hook in self.hooks.list() {
                    match hook.on_pre_inference(&hook_ctx, &mut model_request).await? {
                        InferenceHookOutcome::Continue => {}
                        InferenceHookOutcome::Deny(reason) => {
                            return Err(BellowsError::Workflow(format!(
                                "hook `{}` denied inference: {reason}",
                                hook.name()
                            )));
                        }
                        InferenceHookOutcome::Stub(msg) => {
                            stub = Some(msg);
                            break;
                        }
                    }
                }
                if let Some(msg) = stub {
                    sink.emit(StreamEvent::TurnStart { turn }).await?;
                    if !msg.content.is_empty() {
                        sink.emit(StreamEvent::TextDelta {
                            turn,
                            delta: msg.content.clone(),
                        })
                        .await?;
                    }
                    self.session.push(msg.clone());
                    return Ok(msg);
                }
            }

            sink.emit(StreamEvent::TurnStart { turn }).await?;

            let span = tracing::info_span!(
                parent: &self.trace,
                "inference.turn.streaming",
                turn,
                model = %req.model,
            );
            let mut stream = async { self.model.stream(model_request).await }
                .instrument(span.clone())
                .await?;

            // Accumulators rebuilt fresh each turn.
            let mut text_accum = String::new();
            // Tool calls keyed by id; argument JSON is appended as
            // ToolCallDelta events arrive.
            let mut tool_buffers: Vec<ToolCallBuffer> = Vec::new();
            let mut stop_reason: Option<StopReason> = None;
            let mut usage: Option<ModelUsage> = None;

            while let Some(item) = stream.next().await {
                match item? {
                    ModelStreamEvent::TextDelta { text } => {
                        if !text.is_empty() {
                            text_accum.push_str(&text);
                            sink.emit(StreamEvent::TextDelta { turn, delta: text })
                                .await?;
                        }
                    }
                    ModelStreamEvent::ToolCallStart { id, name } => {
                        tool_buffers.push(ToolCallBuffer {
                            id,
                            name,
                            args_buf: String::new(),
                        });
                    }
                    ModelStreamEvent::ToolCallDelta { id, arguments_json } => {
                        if let Some(buf) = tool_buffers.iter_mut().find(|b| b.id == id) {
                            buf.args_buf.push_str(&arguments_json);
                        } else {
                            tracing::debug!(
                                tool_id = %id,
                                "stream: tool_call_delta for unknown id (no prior tool_call_start)"
                            );
                        }
                    }
                    ModelStreamEvent::EndTurn {
                        stop_reason: sr,
                        usage: u,
                    } => {
                        stop_reason = Some(sr);
                        usage = u;
                    }
                }
            }

            let stop = stop_reason.unwrap_or(StopReason::EndTurn);
            let tool_calls: Vec<ToolCall> = tool_buffers
                .into_iter()
                .map(|b| ToolCall {
                    id: b.id,
                    name: b.name,
                    arguments: parse_tool_args(&b.args_buf),
                })
                .collect();

            let assistant_msg = Message {
                role: MsgRole::Assistant,
                content: text_accum,
                tool_calls: tool_calls.clone(),
                tool_results: Vec::new(),
            };

            // Synthesize a ModelResponse for post-inference hooks. Hooks
            // see the full accumulated message, never partials — same as
            // the contract specifies.
            if !self.hooks.is_empty() {
                let response = ModelResponse {
                    message: assistant_msg.clone(),
                    stop_reason: stop,
                    usage: usage.clone(),
                };
                let hook_ctx = self.hook_ctx();
                for hook in self.hooks.list() {
                    if let HookOutcome::Deny(reason) =
                        hook.on_post_inference(&hook_ctx, &response).await?
                    {
                        return Err(BellowsError::Workflow(format!(
                            "hook `{}` denied response: {reason}",
                            hook.name()
                        )));
                    }
                }
            }

            self.session.push(assistant_msg.clone());

            match stop {
                StopReason::ToolUse => {
                    if tool_calls.is_empty() {
                        return Ok(assistant_msg);
                    }
                    let results = self
                        .dispatch_tool_calls_with_sink(&tool_calls, Some(sink), turn)
                        .await?;
                    self.session.push(Message {
                        role: MsgRole::Tool,
                        content: String::new(),
                        tool_calls: Vec::new(),
                        tool_results: results,
                    });
                }
                StopReason::EndTurn
                | StopReason::MaxTokens
                | StopReason::StopSequence
                | StopReason::Other => {
                    return Ok(assistant_msg);
                }
            }
        }

        Err(BellowsError::Workflow(format!(
            "run_inference_streaming: exceeded max_turns ({}) without a final stop",
            req.max_turns
        )))
    }
}

/// Per-call buffer for tool argument JSON streamed via
/// [`ModelStreamEvent::ToolCallDelta`] events. Held only inside one
/// turn of `run_inference_streaming`; collapsed into a `ToolCall` at
/// turn end.
struct ToolCallBuffer {
    id: String,
    name: String,
    args_buf: String,
}

/// Parse a streamed tool-argument JSON buffer. Falls back to
/// `serde_json::Value::Null` if the buffer is empty (model started a
/// tool call but the stream truncated before any deltas) and to a
/// `{ "error": ... }` envelope on parse failure so the tool sees a
/// well-formed value rather than panicking.
fn parse_tool_args(buf: &str) -> serde_json::Value {
    if buf.trim().is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str(buf) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, raw = %buf, "tool argument JSON did not parse — passing through as error envelope");
            serde_json::json!({ "_invalid_json": buf, "_parse_error": e.to_string() })
        }
    }
}
