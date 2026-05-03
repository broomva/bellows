# Changelog

All notable changes to this workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — phone-friendly web UI (v0.2-pre)

- `GET /` on `bellows-server` now serves a baked-in HTML UI suitable
  for phones (responsive, dark-mode, vanilla JS, no build step). The
  workflow name and an example input are templated in at boot.
- `GET /v1/agents` lists the mounted workflow + its endpoint.
- `Server::with_example_input(json)` builder lets each example pre-fill
  the UI's request-body box with a sensible default.
- `repo-scout` example now branches on `BELLOWS_MODE=server` to spin
  up the HTTP server with all four hooks (TracingHook, CountingHook,
  PathPolicyHook, AllowDenyHook) attached.
- Path-param syntax migrated to axum 0.8 (`/v1/agents/{name}` was
  panicking under axum 0.8; now fixed).
- New `docs/RUNNING.md` documents three deploy paths: CLI one-shot,
  local HTTP server, public URL via cloudflared quick tunnel /
  Tailscale Funnel / ngrok / same-WiFi LAN IP. Includes a security
  note on the no-auth tunnel posture.

### Validated

- Local: `curl /healthz`, `/v1/agents`, `GET /` (HTML), and a real
  `POST /v1/agents/repo-scout` against Claude — all 200 OK; 6-turn
  agent run with 6 tool calls, structured JSON response with full
  hook counters. UI loads with workflow name + example input
  pre-filled.
- Public: `cloudflared tunnel --url http://localhost:3548` opened a
  `*.trycloudflare.com` HTTPS URL with `Registered tunnel connection
  ... protocol=quic` confirmed by cloudflared. The same agent
  endpoint is reachable from any device on the public internet over
  HTTPS via Cloudflare's edge.

### Added — lifecycle hooks (v0.2-pre)

The Bellows analogue of Claude Code's `.claude/settings.json` event
pipeline (`PreToolUse`, `PostToolUse`, `SessionStart`, `Stop`,
`Notification`). In-process Rust trait callbacks rather than shell-out
hooks — fast, type-safe, share the agent's trust posture.

- **`bellows_core::Hook`** trait with eight lifecycle methods, all
  defaulting to `Continue`:
  `on_workflow_start`, `on_workflow_end`,
  `on_step_start`, `on_step_end`,
  `on_pre_inference`, `on_post_inference`,
  `on_pre_tool_use`, `on_post_tool_use`.
- **Outcome enums** typed per-event:
  `HookOutcome` (continue / deny),
  `ToolHookOutcome` (continue / deny / **stub** synthetic result),
  `InferenceHookOutcome` (continue / deny / **stub** synthetic message).
- **`HookRegistry`** — ordered collection walked in registration order.
- **Reference impls in core** (no I/O, just `tracing`):
  - `TracingHook` — emits a `tracing::info!` at every event.
  - `AllowDenyHook` — pattern-match allow/deny by tool name.
- **`Engine::with_hook` / `Engine::with_hooks`** builders.
- **`Server::with_hook`** delegates to the underlying engine.
- **`StepCtx` carries `hooks: Arc<HookRegistry>` + `workflow_name`**
  so the autonomous loop can fire pre/post hooks at the four points
  inside `run_inference` and `step`.
- Tool-use and inference hooks may **mutate** the request/call before
  it executes; post-hooks may mutate the result (e.g. redact).
- Tool-use hooks may **stub** a synthetic result instead of running
  the tool — useful for caching, mocking in tests, and approval-then-
  replace flows.
- Hook errors during pre-events propagate; errors during post-events
  are logged but do not override the underlying action's result
  (best-effort observation).
- Four new unit tests in `bellows-core::hook::tests` for allow/deny
  semantics and registration order — workspace test count 6 → 10.

### Validated end-to-end with real Claude (hooks)

- **Observation scenario:** TracingHook + CountingHook registered;
  every lifecycle event emitted to stderr and counted. `repo-scout`
  reports `pre_inference: 3, post_inference: 3, pre_tool_use: 4,
  post_tool_use: 4` for a 3-turn / 4-tool-call run — exact match
  with the runtime trace.
- **Deny scenario:** `PathPolicyHook` registered with deny-patterns
  `[".env", "id_rsa", "credentials", "secrets"]`. Asked Claude to
  read both `README.md` (allowed) and `.env` (denied). Hook
  intercepted on `on_pre_tool_use`, runtime synthesized
  `tool_result { is_error: true, denied_by: "hook" }`. Claude read
  the deny reason in the next turn and adapted: final answer
  explained the README contents AND that "*the .env file could not
  be read because it matches a deny-pattern in the path policy*".
  `hook_events.denied_paths: [".env"]` captured the audit trail.

### Added — autonomous tool-use loop (v0.2-pre)

- **`StepCtx::step()`** — scopes a child [`Step`] under its own tracing
  span and delegates to the step's `run` body. The canonical way for
  `Workflow::execute` to orchestrate inner steps.
- **`StepCtx::run_inference()`** — drives the autonomous loop:
  build `ModelRequest` → call provider → if `tool_use`, dispatch each
  tool against the sandbox and append a `MsgRole::Tool` results
  message → repeat until a non-tool stop reason or `max_turns` hits.
  Errors from individual tools surface to the model as
  `tool_result { is_error: true }` so the agent can recover rather than
  the workflow crashing.
- **`InferenceRequest`** builder — `with_role`, `with_max_tokens`,
  `with_temperature`, `with_max_turns`. `DEFAULT_INFERENCE_MAX_TURNS = 16`.
- **`AnthropicProvider`** now serialises `ModelRequest::tools` as
  Anthropic's `input_schema` format and re-emits assistant `tool_use`
  blocks alongside subsequent `tool_result` blocks so call ids correlate.
- **Transport-error chaining** — `BellowsError::Model` includes the
  full `source` chain when `reqwest` fails (was hiding the underlying
  hyper / connection cause).
- **`examples/repo-scout`** — end-to-end demo of the autonomous tool
  loop. Claude calls `fs_list` and `fs_read` against `LocalSandbox`,
  inspects an arbitrary repo path, and returns a structured summary
  with the files it actually opened.

### Validated — autonomous tool loop

- **Real Claude executed real tool calls** via the bellows-core dispatch
  loop. Two scenarios:
  - *"Summarize this project"* → 3 turns, `fs_read` x3 (README,
    Cargo.toml, LICENSE), correct workspace summary.
  - *"What traits does bellows-core define?"* → 4 turns, `fs_read` x4
    (crate README, Cargo.toml, lib.rs, workflow.rs), accurate
    enumeration of all 7 traits + the dependency-posture rule.
- Session history correctly threaded `tool_use` ↔ `tool_result` pairs
  across multiple turns; tool errors round-trip as
  `is_error: true` results without aborting the loop.

### Added

- **`bellows-model::AnthropicProvider`** — real Messages API connector.
  Two auth modes: `AnthropicAuth::ApiKey` (`x-api-key` header for
  `sk-ant-api03-...` keys) and `AnthropicAuth::OAuthBearer`
  (`Authorization: Bearer` + `anthropic-beta: oauth-...` for Claude Code
  subscription tokens). `AnthropicAuth::from_env()` resolves either.
- Example `issue-triage` now uses `AnthropicProvider` when credentials
  are present, falls back to `MockProvider` otherwise. Output now
  carries the resolved provider id and reported token usage.
- Workspace lint config: explicit `allow` list for cosmetic clippy
  pedantic noise; `unwrap_used`/`expect_used`/`panic`/`dbg_macro`
  remain `deny` in non-test code.

### Changed

- `RoleScope::Agent` now uses `#[default]` (was a manual `Default` impl).
- `Role::merge` precedence selection inlined to avoid `or_fun_call`.
- `ModelProvider::id` returns `&'static str` (was `&str`).
- `Sandbox::name` for `VirtualSandbox` returns `&'static str`.
- `rustfmt.toml`: removed nightly-only `imports_granularity` /
  `group_imports` keys.

### Validated

- **End-to-end against real Claude** (claude-haiku-4-5 via the OAuth
  bearer auth path). Three distinct scenarios returned correctly
  classified, structured JSON output with token-usage reporting.
- All gates pass: `cargo check`, `cargo test --workspace --lib` (6/6),
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check`.

## [0.1.0-pre] — 2026-05-03

### Added

- **`bellows-core`** kernel contract: `Workflow`, `Step`, `StepCtx`,
  `Session`, `SessionStore`, `Sandbox`, `Tool`, `ToolRegistry`, `Skill`,
  `SkillSet`, `ModelProvider`, `Role` (with `Role::merge` precedence),
  `Message`, `MsgRole`, `ToolCall`, `ToolResult`, `BellowsError`.
- **`bellows-skill`** with `parse_skill`, `load_dir`, `SkillBundle`.
- **`bellows-session::MemoryStore`**.
- **`bellows-model::MockProvider`** for examples + tests.
- **`bellows-sandbox::VirtualSandbox`** (in-process, no exec).
- **`bellows-sandbox-local::LocalSandbox`** with cwd jail, env-clear,
  per-call timeout.
- **`bellows-tool`** with `BashTool`, `FsReadTool`, `FsWriteTool`,
  `FsListTool`, `SimpleRegistry`.
- **`bellows-runtime::Engine::run`** — single-step orchestration scaffold.
- **`bellows-server`** — axum server with `/healthz` and
  `/v1/agents/:name`. Default port: 3548.
- **`bellows-cli`** — `bellows version` + `bellows doctor`.
- **`examples/issue-triage`** — end-to-end example agent.
- Governance: `README.md`, `CLAUDE.md`, `AGENTS.md`, `METALAYER.md`,
  `.control/policy.yaml`, `.control/plant.yaml`, `schemas/`.
- Docs: `ARCHITECTURE.md`, `DEPENDENCY-CHAIN.md`, `ROADMAP.md`,
  `GLOSSARY.md`, `SANDBOX-POSTURE.md`.
- Workspace tooling: `rust-toolchain.toml` (1.85), `rustfmt.toml`,
  `deny.toml`, `Makefile`, `.gitignore`.

### Known limitations (closing in v0.2)

- `Step::run` autonomous loop is scaffolded but does not yet drive
  multi-turn model + tool interaction. v0.1 examples use single-shot
  `ModelProvider::complete`.
- No real LLM provider yet — only `MockProvider`.
- No `bellows build` codegen — `bellows-cli` exposes `version` and
  `doctor` only.
- No proc-macros — `bellows-macros` lands in v0.2.
- No MCP support — gated until v0.2 behind the `mcp` feature flag.
- No Docker / namespace / E2B sandboxes — only `local` and `virtual`.
- No streaming HTTP responses — non-streaming only.

[Unreleased]: https://github.com/broomva/bellows/compare/v0.1.0-pre...HEAD
[0.1.0-pre]: https://github.com/broomva/bellows/releases/tag/v0.1.0-pre
