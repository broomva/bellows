# Changelog

All notable changes to this workspace are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
