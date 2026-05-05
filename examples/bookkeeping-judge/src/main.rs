//! Bellows example: a Broomva knowledge-graph promotion judge.
//!
//! This is a sibling of the Python `skills/bookkeeping/scripts/bookkeeping.py`
//! pipeline — it does NOT replace it. The pipeline today scores items with
//! a heuristic-only pass (the LLM-judge stub depends on
//! `google-generativeai`, which is not installed). This Bellows agent fills
//! that gap by asking Claude to score each `## Item N` block in a raw
//! extract under `research/notes/*-raw.md` against the Nous rubric:
//!
//! - **novelty**     0..=3 — does this introduce something the graph
//!   doesn't already cover?
//! - **specificity** 0..=3 — is the claim grounded with concrete details
//!   (entities, numbers, names) or fluff?
//! - **relevance**   0..=3 — does it bear on Broomva / Life / Agent OS?
//!
//! Total >= 5/9 promotes to Layer 3. Total >= 7/9 is priority + a candidate
//! for a synthesis blog post (`blog_candidate: true`).
//!
//! ## I/O shape
//!
//! Input:
//! ```json
//! { "extract_path": "/abs/path/to/research/notes/2026-05-03-foo-raw.md",
//!   "max_items": 20 }
//! ```
//!
//! Output:
//! ```json
//! { "items": [
//!     { "item_number": 1, "slug": "flue", "type": "tool",
//!       "novelty": 1, "specificity": 3, "relevance": 3,
//!       "total": 7, "pass": true, "blog_candidate": true,
//!       "reasoning": "Flue itself is well-known but the harness-vs-SDK
//!                     distinction is concretely articulated and directly
//!                     informs Bellows positioning." }
//!   ],
//!   "source_file": "/abs/.../2026-05-03-foo-raw.md",
//!   "judged_at": "2026-05-03T17:42:11Z"
//! }
//! ```
//!
//! ## Tools
//!
//! `fs_read` only. The agent has to crack the file open to score it. An
//! `AllowDenyHook::allow_only(["fs_read"])` enforces this — even if the
//! model tries to call `bash` or `fs_write`, the hook intervenes.

use std::sync::Arc;

use async_trait::async_trait;
use bellows_core::{
    AllowDenyHook, BellowsError, InferenceRequest, Message, ModelProvider, Result, Role, Sandbox,
    StepCtx, Tool, TracingHook, Workflow, skill::EmptySkillSet,
};
use bellows_model::{AnthropicAuth, AnthropicProvider, MockProvider};
use bellows_sandbox_local::LocalSandbox;
use bellows_tool::FsReadTool;
use serde::{Deserialize, Serialize};

// ── I/O shape ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JudgeInput {
    /// Absolute path to a raw extract markdown file. The agent will
    /// `fs_read` this through its sandbox.
    pub extract_path: String,
    /// Optional cap on items judged in one run. Useful for very long
    /// extracts; defaults to 50.
    #[serde(default)]
    pub max_items: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct JudgeOutput {
    /// One judgment per `## Item N` block scored.
    pub items: Vec<JudgedItem>,
    /// Echo of the extract path, for downstream callers.
    pub source_file: String,
    /// ISO-8601 UTC timestamp of when the model finished.
    pub judged_at: String,
    /// `"anthropic"` or `"mock"`.
    pub provider: String,
    /// Stable id of the session that produced this output.
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgedItem {
    /// 1-indexed item number from the source file's `## Item N` heading.
    pub item_number: u32,
    /// Proposed entity slug (kebab-case, lowercase).
    pub slug: String,
    /// One of: concept, pattern, tool, person, project, discovery, question.
    #[serde(rename = "type")]
    pub kind: String,
    /// 0..=3.
    pub novelty: u8,
    /// 0..=3.
    pub specificity: u8,
    /// 0..=3.
    pub relevance: u8,
    /// novelty + specificity + relevance (0..=9).
    pub total: u8,
    /// total >= 5.
    pub pass: bool,
    /// total >= 7.
    pub blog_candidate: bool,
    /// One- or two-sentence justification.
    pub reasoning: String,
}

// ── Workflow ─────────────────────────────────────────────────────────────────

pub struct BookkeepingJudge {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
    model_id: String,
}

impl BookkeepingJudge {
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
impl Workflow for BookkeepingJudge {
    type Input = JudgeInput;
    type Output = JudgeOutput;

    fn name(&self) -> &str {
        "bookkeeping-judge"
    }

    fn role(&self) -> Role {
        Role::default()
            .with_identity(
                "You are a strict knowledge-graph promotion judge for the Broomva workspace. \
                 You score raw research extracts against the Nous rubric and decide which \
                 items earn a Layer-3 entity page.",
            )
            .with_instruction(
                "Score every `## Item N` block on three axes: \
                 novelty (0..=3 — does this introduce something the graph doesn't already cover?), \
                 specificity (0..=3 — is the claim grounded in concrete entities, numbers, names, code? \
                 or is it fluff?), \
                 relevance (0..=3 — does it bear on Broomva / Life Agent OS / RCS / Bellows / \
                 Haima / agentic substrate work?). \
                 Total >= 5/9 promotes to Layer 3. Total >= 7/9 is priority + blog_candidate=true.",
            )
            .with_instruction(
                "Be specific in `reasoning`. Cite the concrete signal — \"mentions x402 + USDC \
                 settlement on Base testnet\" — not vague summaries. \
                 If an item is too short to score, give it 0/0/0 and explain.",
            )
            .with_instruction(
                "Pick a kebab-case `slug` and `type` (one of: concept, pattern, tool, person, \
                 project, discovery, question) for each item. The slug should match what an \
                 entity page would be filed as.",
            )
            .with_instruction(
                "Use the `fs_read` tool ONCE on `extract_path` to load the raw extract. \
                 Then return your full JSON judgment. Do not use any other tool. \
                 Do not call `fs_read` more than necessary.",
            )
            .with_instruction(
                "Respond with ONLY a single-line JSON object, no prose, no code fences. \
                 Shape: {\"items\":[{\"item_number\":<u32>,\"slug\":\"...\",\"type\":\"...\",\
                 \"novelty\":<0-3>,\"specificity\":<0-3>,\"relevance\":<0-3>,\
                 \"total\":<0-9>,\"pass\":<bool>,\"blog_candidate\":<bool>,\
                 \"reasoning\":\"...\"}]}",
            )
    }

    fn skills(&self) -> &dyn bellows_core::SkillSet {
        &EmptySkillSet
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(FsReadTool)]
    }

    fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.sandbox.clone()
    }

    fn model(&self) -> Arc<dyn ModelProvider> {
        self.model.clone()
    }

    async fn execute(&self, ctx: &mut StepCtx<'_>, input: JudgeInput) -> Result<JudgeOutput> {
        let max_items = input.max_items.unwrap_or(50);

        let user_msg = format!(
            "Score the FIRST {cap} `## Item N` blocks in the file at `{path}`. \
             Hard cap: do NOT score more than {cap} items even if more exist; \
             stop and return JSON when you reach the cap.\n\n\
             For each item, output one JSON judgment in the items array. \
             Keep `reasoning` to 1-2 sentences per item — terse, concrete, \
             cites the load-bearing detail. \
             Return only the JSON object — no prose, no code fences.\n\n\
             Use fs_read once to load the file, then emit the full judgment.",
            path = input.extract_path,
            cap = max_items,
        );
        ctx.session.push(Message::user(user_msg));

        let req = InferenceRequest::new(&self.model_id)
            .with_role(self.role())
            .with_max_tokens(8192)
            .with_temperature(0.0)
            .with_max_turns(6);
        let final_msg = ctx.run_inference(&req).await?;

        let raw = final_msg.content.trim().to_string();
        let cleaned = strip_code_fence(&raw);
        let parsed: serde_json::Value = serde_json::from_str(cleaned).map_err(|e| {
            BellowsError::Workflow(format!(
                "model did not return valid JSON: {e}; raw output was: {raw}"
            ))
        })?;

        let items_val = parsed
            .get("items")
            .ok_or_else(|| BellowsError::Workflow(format!("missing `items` key in JSON: {raw}")))?;
        let items: Vec<JudgedItem> = serde_json::from_value(items_val.clone()).map_err(|e| {
            BellowsError::Workflow(format!("could not deserialize items[]: {e}; raw: {raw}"))
        })?;

        // Validate the model's arithmetic. We trust but verify — clamp scores
        // to 0..=3, recompute total/pass/blog_candidate, so callers can
        // unconditionally trust the output.
        let items: Vec<JudgedItem> = items
            .into_iter()
            .map(|it| {
                let novelty = it.novelty.min(3);
                let specificity = it.specificity.min(3);
                let relevance = it.relevance.min(3);
                let total = novelty + specificity + relevance;
                JudgedItem {
                    item_number: it.item_number,
                    slug: it.slug,
                    kind: it.kind,
                    novelty,
                    specificity,
                    relevance,
                    total,
                    pass: total >= 5,
                    blog_candidate: total >= 7,
                    reasoning: it.reasoning,
                }
            })
            .collect();

        Ok(JudgeOutput {
            items,
            source_file: input.extract_path,
            judged_at: now_iso(),
            provider: ctx.model.id().to_string(),
            session_id: ctx.session.id.to_string(),
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

/// RFC-3339 / ISO-8601 timestamp without an extra `chrono` dep. The format
/// matches what `bookkeeping.py` already writes elsewhere in the graph.
fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Compute UTC YYYY-MM-DDTHH:MM:SSZ from epoch seconds without a calendar
    // crate. Civil-from-days based on Howard Hinnant's algorithm.
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let h = secs_of_day / 3_600;
    let m = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ── Glue ─────────────────────────────────────────────────────────────────────

fn choose_model() -> String {
    // Default to Haiku 4.5 — fast + cheap for batch judging.
    std::env::var("BELLOWS_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".to_string())
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

    // Sandbox root: we want to allow the agent to read absolute paths into
    // the user's broomva workspace. LocalSandbox honors absolute paths
    // verbatim (see `LocalSandbox::resolve`). Use the current dir as the
    // nominal workspace.
    let workspace = std::env::current_dir().unwrap_or_default();
    let workflow = BookkeepingJudge::new(model, workspace, model_id);

    // Read JSON input from stdin. The Python CLI shells out to us with
    // `JudgeInput` over stdin and consumes JSON over stdout — keep stderr
    // for tracing (see `tracing_subscriber::with_writer(stderr)` above).
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok();
    if buf.trim().is_empty() {
        let err: Box<dyn std::error::Error> = Box::new(BellowsError::Workflow(
            "bookkeeping-judge: no JSON input on stdin".to_string(),
        ));
        return Err(err);
    }
    let input: JudgeInput = serde_json::from_str(&buf).map_err(BellowsError::from)?;

    let store = Arc::new(bellows_session::MemoryStore::new());
    let engine = bellows_runtime::Engine::new(workflow, store)
        .with_hook(Arc::new(TracingHook))
        .with_hook(Arc::new(
            AllowDenyHook::allow_only(["fs_read"])
                .with_reason("bookkeeping-judge only permits fs_read"),
        ));

    let out = engine.run(input).await?;
    let json = serde_json::to_string(&out).map_err(BellowsError::from)?;
    println!("{json}");
    Ok(())
}
