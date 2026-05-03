//! Bellows example: an issue-triage agent.
//!
//! Demonstrates the framework end-to-end:
//!
//! - A user-defined `Workflow` (`IssueTriage`)
//! - The role overlay precedence (workflow-default role + optional caller role)
//! - `LocalSandbox` for filesystem access (the agent reads context from disk)
//! - Real Anthropic provider (Messages API) when credentials are present;
//!   falls back to `MockProvider` when they are not — so the example always
//!   runs.
//! - JSON-shaped output suitable for piping into other tools
//!
//! Run modes:
//! - default (CLI one-shot):   cargo run -p bellows-example-issue-triage
//! - HTTP server on port 3548: BELLOWS_MODE=server cargo run -p bellows-example-issue-triage
//!
//! Provide a real issue:
//!   BELLOWS_ISSUE_TITLE="Tests fail on Windows under tokio 1.42" \
//!   BELLOWS_ISSUE_BODY="$(cat path/to/body.md)" \
//!   cargo run -p bellows-example-issue-triage

use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{
    BellowsError, ModelProvider, ModelRequest, Result, Role, Sandbox, StepCtx, Tool, Workflow,
    skill::EmptySkillSet,
};
use bellows_model::{AnthropicAuth, AnthropicProvider, MockProvider};
use bellows_runtime::Engine;
use bellows_sandbox_local::LocalSandbox;
use bellows_session::MemoryStore;
use bellows_tool::{BashTool, FsListTool, FsReadTool, FsWriteTool};
use serde::{Deserialize, Serialize};

// ── Workflow shape ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TriageInput {
    /// Short issue title.
    pub title: String,
    /// Issue body text.
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct TriageOutput {
    /// Single classification label.
    pub label: String,
    /// Three-priority bucket: low / medium / high.
    pub priority: String,
    /// One-paragraph human-readable summary.
    pub summary: String,
    /// Stable id of the session that produced this output.
    pub session_id: String,
    /// Provider identifier (anthropic | mock) — useful for asserting the
    /// real path was exercised.
    pub provider: String,
    /// Total token usage if the provider reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageReport>,
}

#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// ── Workflow impl ────────────────────────────────────────────────────────────

pub struct IssueTriage {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
}

impl IssueTriage {
    pub fn new(model: Arc<dyn ModelProvider>, workspace: std::path::PathBuf) -> Self {
        Self {
            model,
            sandbox: Arc::new(LocalSandbox::new(workspace)),
        }
    }
}

#[async_trait]
impl Workflow for IssueTriage {
    type Input = TriageInput;
    type Output = TriageOutput;

    fn name(&self) -> &str {
        "issue-triage"
    }

    fn role(&self) -> Role {
        Role::default()
            .with_identity("Issue-triage agent for the Bellows project.")
            .with_instruction(
                "Read the issue title and body. Classify it as one of: \
                 bug, feature, question, docs, ci, chore. Assign a priority \
                 from {low, medium, high} based on user impact. Produce a \
                 one-paragraph summary capturing the actionable point.",
            )
            .with_instruction(
                "Respond with ONLY a JSON object on a single line, no prose, \
                 no code fences. Shape: \
                 {\"label\":\"<one of the labels>\",\"priority\":\"<low|medium|high>\",\"summary\":\"<one paragraph>\"}.",
            )
    }

    fn skills(&self) -> &dyn bellows_core::SkillSet {
        &EmptySkillSet
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(BashTool),
            Arc::new(FsReadTool),
            Arc::new(FsWriteTool),
            Arc::new(FsListTool),
        ]
    }

    fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.sandbox.clone()
    }

    fn model(&self) -> Arc<dyn ModelProvider> {
        self.model.clone()
    }

    async fn execute(&self, ctx: &mut StepCtx<'_>, input: Self::Input) -> Result<Self::Output> {
        // Build the user message — the agent sees both title and body.
        let user_msg = format!(
            "Issue title: {}\n\nIssue body:\n{}",
            input.title.trim(),
            input.body.trim()
        );
        ctx.session.push(bellows_core::Message::user(user_msg));

        let req = ModelRequest {
            model: choose_model(),
            messages: ctx.session.history.clone(),
            role: Some(self.role()),
            tools: Vec::new(), // tool-use loop lands in v0.2
            max_tokens: Some(512),
            temperature: Some(0.0),
            stop: Vec::new(),
        };

        let resp = ctx.model.complete(req).await?;
        ctx.session.push(resp.message.clone());

        // Parse the model's JSON output. Be defensive: strip optional
        // code fences a model might add despite instructions.
        let raw = resp.message.content.trim().to_string();
        let cleaned = strip_code_fence(&raw);
        let parsed: serde_json::Value = serde_json::from_str(cleaned).map_err(|e| {
            BellowsError::Workflow(format!(
                "model did not return valid JSON: {e}; raw output was: {raw}"
            ))
        })?;

        let label = parsed
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let priority = parsed
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("low")
            .to_string();
        let summary = parsed
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)")
            .to_string();

        Ok(TriageOutput {
            label,
            priority,
            summary,
            session_id: ctx.session.id.to_string(),
            provider: ctx.model.id().to_string(),
            usage: resp.usage.map(|u| UsageReport {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            }),
        })
    }
}

fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start().trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start().trim_end_matches("```").trim();
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

// ── Entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    let (model, kind) = build_provider();
    eprintln!("[bellows] provider: {kind}");
    eprintln!("[bellows] model:    {}", choose_model());

    let workspace = std::env::current_dir().unwrap_or_default();

    if std::env::var("BELLOWS_MODE").as_deref() == Ok("server") {
        let workflow = IssueTriage::new(model, workspace);
        bellows_server::serve(workflow).await?;
    } else {
        let workflow = IssueTriage::new(model, workspace);
        run_once(workflow).await?;
    }
    Ok(())
}

async fn run_once(workflow: IssueTriage) -> Result<()> {
    let store = Arc::new(MemoryStore::new());
    let engine = Engine::new(workflow, store);

    let title = std::env::var("BELLOWS_ISSUE_TITLE")
        .unwrap_or_else(|_| "Tests fail on Windows under tokio 1.42".to_string());
    let body = std::env::var("BELLOWS_ISSUE_BODY").unwrap_or_else(|_| {
        "When I run `cargo test --workspace` on Windows 11 with Rust 1.85, \
         the build succeeds but every async test panics with \
         'thread 'tokio-runtime-worker' panicked at: failed to lookup address \
         information: Os { code: 11001 ... }'. Reproduces 100%. \
         macOS and Ubuntu pass cleanly. Suspecting a Windows-specific DNS \
         resolver path in tokio 1.42 — would appreciate guidance."
            .to_string()
    });

    let out = engine.run(TriageInput { title, body }).await?;

    let json = serde_json::to_string_pretty(&out).map_err(BellowsError::from)?;
    println!("{json}");
    Ok(())
}
