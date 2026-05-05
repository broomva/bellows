//! Bellows example: a real conversational chat agent.
//!
//! Unlike `repo-scout` (which is hardcoded to investigate a code repository
//! every turn), this workflow accepts the full conversation history and
//! replies in chat shape. It still has `fs_list` / `fs_read` / `bash`
//! available — Claude can use them when the user asks to investigate
//! something — but it does NOT force a tool-use loop on every message.
//!
//! Input shape (matches what useChat / AI SDK sends):
//!
//! ```json
//! { "messages": [
//!     { "role": "user",      "content": "hi" },
//!     { "role": "assistant", "content": "hello!" },
//!     { "role": "user",      "content": "what can you do?" }
//! ]}
//! ```
//!
//! Output: `{answer, turns, session_id, provider, files_read[], hook_events}`.

use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{
    AllowDenyHook, BellowsError, InferenceRequest, Message, MsgRole, ModelProvider, Result, Role,
    Sandbox, Session, StepCtx, Tool, TracingHook, Workflow, skill::EmptySkillSet,
};
use bellows_model::{AnthropicAuth, AnthropicProvider, MockProvider};
use bellows_sandbox_local::LocalSandbox;
use bellows_tool::{BashTool, FsListTool, FsReadTool, FsWriteTool};
use serde::{Deserialize, Serialize};

// ── I/O shape ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    /// `"user"` or `"assistant"`. Anything else is treated as user.
    pub role: String,
    /// Plain text content. Tool-call rendering is the workflow's concern, not
    /// the caller's; the wrapper accepts plain text only.
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatInput {
    /// Full conversation history. Last item should be a user message; the
    /// agent replies to that turn given everything before it.
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
pub struct ChatOutput {
    /// Plain-text reply.
    pub answer: String,
    /// Backwards-compat: files Claude opened with `fs_read` during the turn.
    /// Older clients (the v0.2-pre Next.js app) read this; the richer
    /// [`tools`] array below is what new clients should consume.
    pub files_read: Vec<String>,
    /// Every tool invocation made during this turn — name + a short label
    /// derived from arguments + a `denied` flag for hook-blocked calls.
    /// This is what the chat UI's Task widget renders.
    pub tools: Vec<ToolUse>,
    /// Number of model calls in the autonomous loop for this turn.
    pub turns: u32,
    /// Stable id of the session that produced this output.
    pub session_id: String,
    /// `"anthropic"` or `"mock"`.
    pub provider: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolUse {
    /// Tool name as advertised in its `ToolSchema` (e.g. `"fs_list"`).
    pub name: String,
    /// Short human-readable label for the call (typically the path or
    /// shell command). Falls back to the JSON-encoded args for tools we
    /// don't have a special-case formatter for.
    pub label: String,
    /// `true` when an `on_pre_tool_use` hook returned `Deny` and the
    /// runtime synthesised a `tool_result { is_error: true }` instead of
    /// running the tool.
    pub denied: bool,
}

// ── Workflow ─────────────────────────────────────────────────────────────────

pub struct ChatAgent {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
    model_id: String,
}

impl ChatAgent {
    pub fn new(
        model: Arc<dyn ModelProvider>,
        workspace: std::path::PathBuf,
        model_id: String,
    ) -> Self {
        Self {
            model,
            sandbox: Arc::new(LocalSandbox::new(workspace)),
            model_id,
        }
    }
}

#[async_trait]
impl Workflow for ChatAgent {
    type Input = ChatInput;
    type Output = ChatOutput;

    fn name(&self) -> &str {
        "chat-agent"
    }

    fn role(&self) -> Role {
        Role::default()
            .with_identity(
                "You are bellows-chat, a conversational assistant running inside a Rust agent harness. \
                 You're hosted on Railway in a minimal Linux container. Be friendly, concise, and direct.",
            )
            .with_instruction(
                "You have `fs_list`, `fs_read`, and `bash` tools available against a local sandbox. \
                 The sandbox is the working directory of the running container — typically nearly empty. \
                 Use the tools ONLY when the user asks something that genuinely requires inspecting files \
                 or running a command. For greetings, small talk, or general questions, just chat — do not \
                 invoke tools.",
            )
            .with_instruction(
                "When you do use tools, narrate what you're doing in your reply. When the sandbox is empty, \
                 say so honestly rather than fabricating findings.",
            )
            .with_instruction(
                "Keep replies to ~3 sentences unless the user asks for more depth.",
            )
    }

    fn skills(&self) -> &dyn bellows_core::SkillSet {
        &EmptySkillSet
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(BashTool),
            Arc::new(FsListTool),
            Arc::new(FsReadTool),
            Arc::new(FsWriteTool),
        ]
    }

    fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.sandbox.clone()
    }

    fn model(&self) -> Arc<dyn ModelProvider> {
        self.model.clone()
    }

    async fn execute(&self, ctx: &mut StepCtx<'_>, input: ChatInput) -> Result<ChatOutput> {
        // Replay the caller-supplied conversation history into the session.
        // We trust the caller's ordering and roles. Empty messages are
        // skipped so noisy clients don't poison the context.
        for msg in input.messages {
            let trimmed = msg.content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let role = match msg.role.as_str() {
                "assistant" => MsgRole::Assistant,
                _ => MsgRole::User,
            };
            ctx.session.push(Message {
                role,
                content: trimmed.to_string(),
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
            });
        }

        if ctx.session.history.is_empty() {
            return Err(BellowsError::Workflow(
                "chat-agent: messages array was empty".to_string(),
            ));
        }

        // Drive the autonomous loop. Cap at 6 turns — chat replies that need
        // more than 6 model calls almost certainly mean the model is stuck.
        let req = InferenceRequest::new(&self.model_id)
            .with_role(self.role())
            .with_max_tokens(1024)
            .with_temperature(0.6)
            .with_max_turns(6);
        let final_msg = ctx.run_inference(&req).await?;

        let turns = u32::try_from(
            ctx.session
                .history
                .iter()
                .filter(|m| matches!(m.role, MsgRole::Assistant))
                .count(),
        )
        .unwrap_or(u32::MAX);

        // Pair every assistant tool_call with its corresponding tool_result
        // so we know which calls were denied by hooks. Hooks synthesise
        // `tool_result { is_error: true, output: { denied_by: "hook" } }`
        // when they Deny — that's the marker we look for.
        let mut denied_call_ids = std::collections::HashSet::new();
        for msg in &ctx.session.history {
            for tr in &msg.tool_results {
                if !tr.is_error {
                    continue;
                }
                let denied = tr
                    .output
                    .get("denied_by")
                    .and_then(serde_json::Value::as_str)
                    == Some("hook");
                if denied {
                    denied_call_ids.insert(tr.call_id.clone());
                }
            }
        }

        let mut tools_vec: Vec<ToolUse> = Vec::new();
        let mut files_read: Vec<String> = Vec::new();
        for msg in &ctx.session.history {
            for tc in &msg.tool_calls {
                let label = format_tool_label(&tc.name, &tc.arguments);
                if tc.name == "fs_read" {
                    if let Some(path) =
                        tc.arguments.get("path").and_then(serde_json::Value::as_str)
                    {
                        files_read.push(path.to_string());
                    }
                }
                tools_vec.push(ToolUse {
                    name: tc.name.clone(),
                    label,
                    denied: denied_call_ids.contains(&tc.id),
                });
            }
        }

        Ok(ChatOutput {
            answer: final_msg.content,
            files_read,
            tools: tools_vec,
            turns,
            session_id: ctx.session.id.to_string(),
            provider: ctx.model.id().to_string(),
        })
    }
}

/// Build a short human-readable label for a tool call. Specialised for the
/// built-in tools; falls back to compact JSON for unknown tools.
fn format_tool_label(name: &str, args: &serde_json::Value) -> String {
    let s = |key: &str| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "fs_list" => s("path").is_empty().then(|| "(no path)".to_string()).unwrap_or(s("path")),
        "fs_read" => s("path").is_empty().then(|| "(no path)".to_string()).unwrap_or(s("path")),
        "fs_write" => {
            let p = s("path");
            let bytes = args
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(|c| c.len())
                .unwrap_or(0);
            if p.is_empty() {
                format!("(no path, {bytes}B)")
            } else {
                format!("{p} ({bytes}B)")
            }
        }
        "bash" => {
            let cmd = s("cmd");
            if cmd.len() > 80 {
                format!("{}…", &cmd[..80])
            } else {
                cmd
            }
        }
        _ => serde_json::to_string(args)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect(),
    }
}

// ── Glue ─────────────────────────────────────────────────────────────────────

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
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let (model, provider_kind) = build_provider();
    let model_id = choose_model();
    eprintln!("[bellows] provider: {provider_kind}");
    eprintln!("[bellows] model:    {model_id}");

    let workspace = std::env::current_dir().unwrap_or_default();
    let workflow = ChatAgent::new(model, workspace, model_id);

    if std::env::var("BELLOWS_MODE").as_deref() == Ok("server") {
        let example = serde_json::json!({
            "messages": [
                { "role": "user", "content": "hi! what are you?" }
            ]
        });
        let example_str = serde_json::to_string_pretty(&example).map_err(BellowsError::from)?;
        bellows_server::Server::new(workflow)
            .with_example_input(example_str)
            .with_hook(Arc::new(TracingHook))
            .with_hook(Arc::new(AllowDenyHook::deny_list(["bash"]).with_reason(
                "bash is denied by default in chat-agent; ask the operator to enable it",
            )))
            .run()
            .await?;
        return Ok(());
    }

    // CLI one-shot — read JSON input from stdin if available, else use a default.
    use std::io::Read;
    let mut buf = String::new();
    let stdin_attached = !atty_workaround_is_terminal();
    if stdin_attached {
        std::io::stdin().read_to_string(&mut buf).ok();
    }
    let input: ChatInput = if buf.trim().is_empty() {
        ChatInput {
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi! introduce yourself in one sentence.".to_string(),
            }],
        }
    } else {
        serde_json::from_str(&buf).map_err(BellowsError::from)?
    };

    let store = Arc::new(bellows_session::MemoryStore::new());
    let engine = bellows_runtime::Engine::new(workflow, store)
        .with_hook(Arc::new(TracingHook))
        .with_hook(Arc::new(AllowDenyHook::deny_list(["bash"])));

    let out = engine.run(input).await?;
    let json = serde_json::to_string_pretty(&out).map_err(BellowsError::from)?;
    println!("{json}");
    Ok(())
}

// stdin-detached check without an extra crate dep. `is_terminal` is stable in
// std::io::IsTerminal but the result interpretation differs between TTYs and
// piped input — a single helper keeps the call site obvious.
fn atty_workaround_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[allow(dead_code)]
fn _hold(_s: Session) {}
