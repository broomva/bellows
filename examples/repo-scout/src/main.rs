//! Bellows example: a repo-scout agent that exercises the **autonomous
//! tool-use loop**.
//!
//! Unlike `issue-triage` (which makes a single model call), this example
//! demonstrates the full Bellows thesis end-to-end:
//!
//! 1. The user calls `engine.run(input)`.
//! 2. `Workflow::execute` orchestrates a single [`ScoutStep`] via
//!    `ctx.step(...)`.
//! 3. Inside `ScoutStep::run`, the workflow calls `ctx.run_inference(...)`.
//! 4. Claude sees `fs_list` and `fs_read` tools, picks a strategy, and
//!    iterates: list dirs → read files → emit a final structured summary.
//! 5. The runtime dispatches each tool against `LocalSandbox` and feeds
//!    the results back to Claude transparently.
//!
//! The deterministic outer code (the `execute` body) and the autonomous
//! inner code (the `run_inference` loop) are clearly separated. Replace
//! `LocalSandbox` with `DockerSandbox` (v0.2) or `E2BSandbox` (v0.3) and
//! the workflow code does not change.
//!
//! Run:
//! ```text
//! export CLAUDE_CODE_OAUTH_TOKEN=...    # or ANTHROPIC_API_KEY
//! cargo run --release -p bellows-example-repo-scout
//! ```

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use bellows_core::{
    AllowDenyHook, BellowsError, Hook, HookCtx, HookOutcome, InferenceHookOutcome,
    InferenceRequest, Message, ModelProvider, ModelRequest, ModelResponse, Result, Role, Sandbox,
    Step, StepCtx, Tool, ToolCall, ToolHookOutcome, ToolResult, TracingHook, Workflow,
    skill::EmptySkillSet,
};
use bellows_model::{AnthropicAuth, AnthropicProvider, MockProvider};
use bellows_runtime::Engine;
use bellows_sandbox_local::LocalSandbox;
use bellows_session::MemoryStore;
use bellows_tool::{FsListTool, FsReadTool};
use serde::{Deserialize, Serialize};

// ── Workflow shape ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScoutInput {
    /// Path (relative to the sandbox workspace) the agent should investigate.
    pub start_path: String,
    /// What the user wants to learn about the repo.
    pub question: String,
}

#[derive(Debug, Serialize)]
pub struct ScoutOutput {
    /// One-paragraph answer.
    pub answer: String,
    /// Files Claude actually opened during the loop (extracted from
    /// session history — proves the tool path was exercised).
    pub files_read: Vec<String>,
    /// Number of model turns the loop took.
    pub turns: u32,
    /// Stable id of the session that produced this output.
    pub session_id: String,
    /// Provider id (`anthropic` | `mock`).
    pub provider: String,
    /// Hook event counters captured during the run.
    pub hook_events: HookEvents,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct HookEvents {
    pub workflow_starts: u64,
    pub workflow_ends: u64,
    pub step_starts: u64,
    pub pre_inference: u64,
    pub post_inference: u64,
    pub pre_tool_use: u64,
    pub post_tool_use: u64,
    /// Tool calls that the path-policy hook denied.
    pub denied_paths: Vec<String>,
}

// ── Custom hooks for the demo ────────────────────────────────────────────────

/// Counts every lifecycle event the runtime fires. Cheap, lock-free.
#[derive(Debug, Default)]
pub struct CountingHook {
    workflow_starts: AtomicU64,
    workflow_ends: AtomicU64,
    step_starts: AtomicU64,
    pre_inference: AtomicU64,
    post_inference: AtomicU64,
    pre_tool_use: AtomicU64,
    post_tool_use: AtomicU64,
}

#[async_trait]
impl Hook for CountingHook {
    fn name(&self) -> &str {
        "counting"
    }

    async fn on_workflow_start(&self, _: &HookCtx<'_>) -> Result<HookOutcome> {
        self.workflow_starts.fetch_add(1, Ordering::Relaxed);
        Ok(HookOutcome::Continue)
    }
    async fn on_workflow_end(&self, _: &HookCtx<'_>, _: bool) -> Result<HookOutcome> {
        self.workflow_ends.fetch_add(1, Ordering::Relaxed);
        Ok(HookOutcome::Continue)
    }
    async fn on_step_start(&self, _: &HookCtx<'_>, _: &str) -> Result<HookOutcome> {
        self.step_starts.fetch_add(1, Ordering::Relaxed);
        Ok(HookOutcome::Continue)
    }
    async fn on_pre_inference(
        &self,
        _: &HookCtx<'_>,
        _: &mut ModelRequest,
    ) -> Result<InferenceHookOutcome> {
        self.pre_inference.fetch_add(1, Ordering::Relaxed);
        Ok(InferenceHookOutcome::Continue)
    }
    async fn on_post_inference(&self, _: &HookCtx<'_>, _: &ModelResponse) -> Result<HookOutcome> {
        self.post_inference.fetch_add(1, Ordering::Relaxed);
        Ok(HookOutcome::Continue)
    }
    async fn on_pre_tool_use(&self, _: &HookCtx<'_>, _: &mut ToolCall) -> Result<ToolHookOutcome> {
        self.pre_tool_use.fetch_add(1, Ordering::Relaxed);
        Ok(ToolHookOutcome::Continue)
    }
    async fn on_post_tool_use(
        &self,
        _: &HookCtx<'_>,
        _: &ToolCall,
        _: &mut ToolResult,
    ) -> Result<HookOutcome> {
        self.post_tool_use.fetch_add(1, Ordering::Relaxed);
        Ok(HookOutcome::Continue)
    }
}

/// Pre-tool-use hook that denies `fs_read` for paths matching any of a
/// list of substrings. Demonstrates a path-policy guardrail similar to
/// what production systems wire as Claude Code's `PreToolUse` hook.
#[derive(Debug)]
pub struct PathPolicyHook {
    deny_patterns: Vec<String>,
    denied: tokio::sync::Mutex<Vec<String>>,
}

impl PathPolicyHook {
    fn new(patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            deny_patterns: patterns.into_iter().map(Into::into).collect(),
            denied: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn snapshot_denied(&self) -> Vec<String> {
        self.denied.lock().await.clone()
    }
}

#[async_trait]
impl Hook for PathPolicyHook {
    fn name(&self) -> &str {
        "path-policy"
    }

    async fn on_pre_tool_use(
        &self,
        _ctx: &HookCtx<'_>,
        call: &mut ToolCall,
    ) -> Result<ToolHookOutcome> {
        if call.name != "fs_read" {
            return Ok(ToolHookOutcome::Continue);
        }
        let path = call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        for pat in &self.deny_patterns {
            if path.contains(pat) {
                self.denied.lock().await.push(path.to_string());
                return Ok(ToolHookOutcome::Deny(format!(
                    "path-policy refused: `{path}` matches deny-pattern `{pat}`"
                )));
            }
        }
        Ok(ToolHookOutcome::Continue)
    }
}

// ── Step (the autonomous boundary) ───────────────────────────────────────────

pub struct ScoutStep {
    role: Role,
    model: String,
    counter: Arc<CountingHook>,
    path_policy: Arc<PathPolicyHook>,
}

#[async_trait]
impl Step for ScoutStep {
    type Input = ScoutInput;
    type Output = ScoutOutput;

    fn name(&self) -> &str {
        "scout"
    }

    async fn run(&self, ctx: &mut StepCtx<'_>, input: ScoutInput) -> Result<ScoutOutput> {
        // Seed the conversation with the user's request.
        let prompt = format!(
            "You are inspecting a code repository. The workspace root is your \
             current directory. Start at `{}`. Question: {}\n\n\
             Use the `fs_list` tool to enumerate directories and `fs_read` to \
             read individual files (paths are relative to the workspace). \
             Read at most 5 files. Then produce your final answer as a single \
             JSON object with keys `answer` (one paragraph) and `files_read` \
             (array of paths you actually opened, in order). Respond with \
             ONLY that JSON object — no prose, no code fences.",
            input.start_path, input.question
        );
        ctx.session.push(Message::user(prompt));

        // Drive the autonomous loop. The runtime handles tool dispatch.
        let req = InferenceRequest::new(&self.model)
            .with_role(self.role.clone())
            .with_max_tokens(1024)
            .with_temperature(0.0)
            .with_max_turns(10);
        let final_msg = ctx.run_inference(&req).await?;

        // Count turns: every assistant message in the session is one turn.
        let turns = u32::try_from(
            ctx.session
                .history
                .iter()
                .filter(|m| matches!(m.role, bellows_core::MsgRole::Assistant))
                .count(),
        )
        .unwrap_or(u32::MAX);

        // Extract the files the model actually opened by walking
        // tool_calls in the assistant history.
        let files_read = ctx
            .session
            .history
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .filter(|tc| tc.name == "fs_read")
            .filter_map(|tc| {
                tc.arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect::<Vec<_>>();

        // Parse the model's final JSON answer.
        let raw = final_msg.content.trim().to_string();
        let cleaned = strip_code_fence(&raw);
        let parsed: serde_json::Value = serde_json::from_str(cleaned).map_err(|e| {
            BellowsError::Workflow(format!(
                "model did not return valid JSON: {e}; raw output was: {raw}"
            ))
        })?;
        let answer = parsed
            .get("answer")
            .and_then(|v| v.as_str())
            .unwrap_or("(no answer)")
            .to_string();

        // Snapshot hook counters for the output.
        let hook_events = HookEvents {
            workflow_starts: self.counter.workflow_starts.load(Ordering::Relaxed),
            workflow_ends: self.counter.workflow_ends.load(Ordering::Relaxed),
            step_starts: self.counter.step_starts.load(Ordering::Relaxed),
            pre_inference: self.counter.pre_inference.load(Ordering::Relaxed),
            post_inference: self.counter.post_inference.load(Ordering::Relaxed),
            pre_tool_use: self.counter.pre_tool_use.load(Ordering::Relaxed),
            post_tool_use: self.counter.post_tool_use.load(Ordering::Relaxed),
            denied_paths: self.path_policy.snapshot_denied().await,
        };

        Ok(ScoutOutput {
            answer,
            files_read,
            turns,
            session_id: ctx.session.id.to_string(),
            provider: ctx.model.id().to_string(),
            hook_events,
        })
    }
}

// ── Workflow ─────────────────────────────────────────────────────────────────

pub struct RepoScout {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
    model_id: String,
    counter: Arc<CountingHook>,
    path_policy: Arc<PathPolicyHook>,
}

impl RepoScout {
    pub fn new(
        model: Arc<dyn ModelProvider>,
        workspace: std::path::PathBuf,
        model_id: String,
        counter: Arc<CountingHook>,
        path_policy: Arc<PathPolicyHook>,
    ) -> Self {
        Self {
            model,
            sandbox: Arc::new(LocalSandbox::new(workspace)),
            model_id,
            counter,
            path_policy,
        }
    }
}

#[async_trait]
impl Workflow for RepoScout {
    type Input = ScoutInput;
    type Output = ScoutOutput;

    fn name(&self) -> &str {
        "repo-scout"
    }

    fn role(&self) -> Role {
        Role::default()
            .with_identity("Repo-scout — a careful, evidence-driven code investigator.")
            .with_instruction(
                "When the user asks about a repository, list directories with \
                 `fs_list` before reading files, prefer documentation and \
                 manifests (README, Cargo.toml, *.md) before source, and \
                 stop reading once you have enough information.",
            )
    }

    fn skills(&self) -> &dyn bellows_core::SkillSet {
        &EmptySkillSet
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(FsListTool), Arc::new(FsReadTool)]
    }

    fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.sandbox.clone()
    }

    fn model(&self) -> Arc<dyn ModelProvider> {
        self.model.clone()
    }

    async fn execute(&self, ctx: &mut StepCtx<'_>, input: ScoutInput) -> Result<ScoutOutput> {
        // The deterministic outer code: one Step. The autonomous part
        // lives entirely inside ScoutStep::run via ctx.run_inference().
        let step = ScoutStep {
            role: self.role(),
            model: self.model_id.clone(),
            counter: self.counter.clone(),
            path_policy: self.path_policy.clone(),
        };
        ctx.step(&step, input).await
    }
}

// ── Glue ─────────────────────────────────────────────────────────────────────

/// Pull the first balanced JSON object out of a model response. Tolerates
/// optional preamble prose, code fences, and trailing commentary — matches
/// the practical reality that LLMs occasionally chatter despite "respond
/// with ONLY JSON" instructions.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    // Fast path: already pure JSON.
    if s.starts_with('{') {
        return s;
    }
    // Code-fenced JSON anywhere in the string.
    for fence in ["```json", "```"] {
        if let Some(start) = s.find(fence) {
            let after_open = &s[start + fence.len()..];
            let body_start = after_open.find('{').map_or(0, |i| i);
            let body = &after_open[body_start..];
            if let Some(end) = body.rfind("```") {
                return body[..end].trim();
            }
            return body.trim();
        }
    }
    // Last-resort: find a top-level `{...}` span by scanning.
    if let Some(start) = s.find('{') {
        let bytes = s.as_bytes();
        let mut depth: u32 = 0;
        for (i, b) in bytes.iter().enumerate().skip(start) {
            match *b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &s[start..=i];
                    }
                }
                _ => {}
            }
        }
    }
    s
}

fn choose_model() -> String {
    std::env::var("BELLOWS_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".to_string())
}

fn build_provider() -> (Arc<dyn ModelProvider>, &'static str) {
    match AnthropicAuth::from_env() {
        Some(auth) => {
            let kind = match &auth {
                AnthropicAuth::ApiKey(_) => "anthropic (api-key)",
                AnthropicAuth::OAuthBearer(_) => "anthropic (oauth)",
            };
            (Arc::new(AnthropicProvider::new(auth)), kind)
        }
        None => (Arc::new(MockProvider), "mock (no credentials)"),
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,bellows=debug")),
        )
        .compact()
        .init();

    let (model, provider_kind) = build_provider();
    let model_id = choose_model();
    eprintln!("[bellows] provider: {provider_kind}");
    eprintln!("[bellows] model:    {model_id}");

    let workspace = std::env::current_dir().unwrap_or_default();

    // Build hooks. Counter for stats, path-policy for security guardrail,
    // AllowDenyHook to prove the API works (denies fs_write — Claude
    // never has it in this example, but the registration is real),
    // TracingHook for lifecycle observability.
    let counter = Arc::new(CountingHook::default());
    let path_policy = Arc::new(PathPolicyHook::new([
        ".env",
        "id_rsa",
        "credentials",
        "secrets",
    ]));

    let workflow = RepoScout::new(
        model,
        workspace.clone(),
        model_id,
        counter.clone(),
        path_policy.clone(),
    );
    let store = Arc::new(MemoryStore::new());
    let engine = Engine::new(workflow, store)
        .with_hook(Arc::new(TracingHook))
        .with_hook(counter.clone())
        .with_hook(path_policy.clone())
        .with_hook(Arc::new(AllowDenyHook::deny_list(["fs_write"])));

    let start_path = std::env::var("BELLOWS_START_PATH").unwrap_or_else(|_| ".".to_string());
    let question = std::env::var("BELLOWS_QUESTION").unwrap_or_else(|_| {
        "What is this project, what crates does its workspace contain, and what is its license? \
         Answer in one paragraph."
            .to_string()
    });

    let out = engine
        .run(ScoutInput {
            start_path,
            question,
        })
        .await?;

    let json = serde_json::to_string_pretty(&out).map_err(BellowsError::from)?;
    println!("{json}");
    Ok(())
}
