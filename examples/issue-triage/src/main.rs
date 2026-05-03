//! Bellows example: an issue-triage agent.
//!
//! Demonstrates the framework's surface end-to-end:
//!
//! - A user-defined `Workflow` (`IssueTriage`)
//! - Two autonomous `Step`s (`Fetch`, `Classify`)
//! - The role overlay precedence (workflow-default role)
//! - `LocalSandbox` for any future tool calls
//! - `MockProvider` so the example runs without API keys
//! - Either a one-shot CLI invocation (`run_once`) or a long-running HTTP
//!   server on the default Bellows port
//!
//! v0.1 stops short of wiring real LLM tool-loop behavior — that lands in
//! v0.2 alongside `bellows-model`'s Anthropic/OpenAI connectors. The shape
//! you see here is the shape user code keeps as features land.

use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{
    BellowsError, ModelProvider, Result, Role, Sandbox, Session, StepCtx, Tool, Workflow,
    skill::EmptySkillSet,
};
use bellows_model::MockProvider;
use bellows_runtime::Engine;
use bellows_sandbox_local::LocalSandbox;
use bellows_session::MemoryStore;
use bellows_tool::{BashTool, FsListTool, FsReadTool, FsWriteTool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct TriageInput {
    /// GitHub issue URL (or any opaque identifier).
    pub issue_url: String,
}

#[derive(Debug, Serialize)]
pub struct TriageOutput {
    /// Suggested label.
    pub label: String,
    /// One-paragraph summary.
    pub summary: String,
    /// Stable id of the session that produced this output.
    pub session_id: String,
}

pub struct IssueTriage {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
}

impl IssueTriage {
    pub fn new() -> Self {
        Self {
            model: Arc::new(MockProvider),
            sandbox: Arc::new(LocalSandbox::new(std::env::current_dir().unwrap_or_default())),
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
            .with_identity("Issue-triage agent")
            .with_instruction(
                "Read an issue, identify whether it's a bug/feature/question, \
                 and produce a one-paragraph summary plus a single label.",
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
        // v0.1 placeholder: append the input to the session, ask the (mock)
        // model for a stub answer, and return a synthetic triage result.
        // v0.2 will replace this body with `ctx.step(&Fetch).await?` and
        // `ctx.step(&Classify).await?` once the autonomous-step driver
        // is reified in `bellows-runtime`.
        ctx.session.push(bellows_core::Message::user(format!(
            "Please triage issue: {}",
            input.issue_url
        )));
        let req = bellows_core::ModelRequest {
            model: "mock".to_string(),
            messages: ctx.session.history.clone(),
            role: Some(self.role()),
            tools: self.tools().iter().map(|t| t.schema()).collect(),
            max_tokens: None,
            temperature: None,
            stop: Vec::new(),
        };
        let resp = ctx.model.complete(req).await?;
        ctx.session.push(resp.message.clone());

        Ok(TriageOutput {
            label: "needs-triage".to_string(),
            summary: resp.message.content,
            session_id: ctx.session.id.to_string(),
        })
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    // Mode select: BELLOWS_MODE=server runs the HTTP server; default = one-shot CLI.
    if std::env::var("BELLOWS_MODE").as_deref() == Ok("server") {
        bellows_server::serve(IssueTriage::new()).await?;
    } else {
        run_once().await?;
    }
    Ok(())
}

async fn run_once() -> Result<()> {
    let store = Arc::new(MemoryStore::new());
    let engine = Engine::new(IssueTriage::new(), store);
    let out = engine
        .run(TriageInput {
            issue_url: "https://github.com/example/repo/issues/1".to_string(),
        })
        .await?;
    let json = serde_json::to_string_pretty(&out).map_err(BellowsError::from)?;
    println!("{json}");
    Ok(())
}

// Drop a minimal Session helper in scope so the example compiles standalone.
// Avoids needing additional `use` statements in the snippet body above.
#[allow(dead_code)]
fn _unused_session() -> Session {
    Session::new()
}
