# Roadmap

> v0.1 → v1.0 plan with explicit gates.

## v0.1.0-pre — kernel contract published (this commit)

**Status:** ✅ shipped 2026-05-03.

**Goal:** lock the trait set in `bellows-core` and ship a workspace
that compiles, has scaffolding for every layer, and demonstrates the
agent shape end-to-end via `examples/issue-triage/`.

| Component | Done |
|---|---|
| `bellows-core`: full trait set | yes |
| `bellows-skill`: parser + `load_dir` + `SkillBundle` | yes |
| `bellows-session`: `MemoryStore` | yes |
| `bellows-model`: `MockProvider` | yes (mock only) |
| `bellows-sandbox` + `bellows-sandbox-local` | yes |
| `bellows-tool`: bash + fs_read + fs_write + fs_list + `SimpleRegistry` | yes |
| `bellows-runtime`: `Engine::run` (single-step orchestration) | yes (loop scaffolding only) |
| `bellows-server`: axum + `/healthz` + `/v1/agents/:name` | yes |
| `bellows-cli`: `bellows version` + `bellows doctor` | yes |
| `examples/issue-triage`: compiles + runs against `MockProvider` | yes |
| Governance: README, CLAUDE.md, AGENTS.md, METALAYER.md, .control/, schemas/ | yes |
| Docs: ARCHITECTURE.md, DEPENDENCY-CHAIN.md, ROADMAP.md, GLOSSARY.md, SANDBOX-POSTURE.md | yes |

## v0.2 — autonomous loop + real providers + `bellows build`

**Goal:** make Bellows agents *actually run* end-to-end against a real
LLM and produce a deployable artifact.

| Item | Spec |
|---|---|
| **`Step::run` autonomous loop** | the model→tool→model loop wired in `bellows-runtime`. Closes the v0.1 scaffolding gap. |
| **`bellows-model::AnthropicProvider`** | streaming + tool calls + prompt caching. Resolves RF-1. |
| **`bellows-model::OpenAIProvider`** | streaming + tool calls. |
| **`bellows-model::OpenRouterProvider`** | thin wrapper. |
| **`bellows-tool::McpTool`** behind `mcp` feature | adapter that fans MCP server tools into the unified registry. |
| **`bellows-build` crate** | discover workflows via `inventory`, codegen wrapper crate, embed skills via `include_dir!`, shell out to `cargo build --release`. |
| **`bellows-macros` crate** | `#[workflow]`, `#[step]`, `#[tool]` proc-macros. |
| **`bellows-cli` `build` and `run` commands** | wired to `bellows-build`. |
| **`bellows-sandbox-docker`** | bollard-based, feature-gated. |
| **`bellows-session::SqliteStore`** | feature-gated (`sqlite`). |
| **CI: GitHub Actions** | check / test / clippy / fmt / deny matrix on macOS + Linux + Windows. |
| **Snapshot tests via `insta`** | skill parsing, role rendering, model-request building. |

**Definition of done for v0.2:** the issue-triage example compiles into
a `dist/issue-triage` binary that, given an `ANTHROPIC_API_KEY`, can
classify a real GitHub issue end-to-end. Tests prove role precedence
and skill loading are stable across refactors.

## v0.3 — sandbox + MCP + observability hardening

**Goal:** production-readiness for everything the agent touches.

| Item | Spec |
|---|---|
| **`bellows-sandbox-namespaces`** (Linux) | `nix` + `caps` + `landlock` + `seccompiler`. Linux-only feature. |
| **`bellows-sandbox-e2b`** | HTTP client. Feature-gated. |
| **MCP server feature** | expose Bellows tools as an MCP server (via `rmcp` server transport). |
| **OTLP integration tests** | wiremock the OTLP endpoint, assert spans. |
| **Approval system** | per-tool approval policy (`auto-approve` lists, `prompt-on-first-use`, `deny-list` patterns) — Codex-shaped. |
| **Subagent isolation** | `ctx.subagent(W).await` spawns a fresh session and a fresh tool registry; explicit doc on what subagents inherit. |
| **Replay tests** | given a recorded session, re-run with a recorded `MockProvider` and assert deterministic output. |

## v0.4 — performance + parallelism

**Goal:** keep pace with the production workloads users will throw at
the framework.

| Item | Spec |
|---|---|
| **Parallel tool execution** | when an assistant turn emits multiple tool calls, dispatch them concurrently using `FuturesOrdered`. |
| **Streaming HTTP responses** | server endpoint streams `ModelStreamEvent`s as SSE. |
| **Context compaction** | utilities for summarizing older messages to fit token budgets. |
| **Provider fallbacks** | `ChainProvider` that tries providers in order on failure. |
| **Bench suite** | `criterion` benches for skill parse, role merge, sandbox exec dispatch. |

## v1.0 — stable, published, real users

**Goal:** ship `bellows-core` 1.0 with a contract we are willing to
support for years.

| Gate | Spec |
|---|---|
| Trait stability | every `bellows-core` trait has been used by at least 3 different non-trivial workflows. |
| Provider coverage | Anthropic + OpenAI + OpenRouter + at least one local provider (Ollama or `llama.cpp` via HTTP). |
| Sandbox coverage | local + Docker + namespaces + remote (E2B). |
| Documentation | full rustdoc on every public item; tutorial walkthrough; design-doc series. |
| Tests | clippy clean, deny clean, msrv-check clean, 80%+ branch coverage on `bellows-core` and `bellows-runtime`. |
| External users | at least one external project (outside Broomva) building agents on Bellows. |
| Crates.io release | every crate published, versions aligned to `1.0.0`. |
| Governance | METALAYER setpoints calibrated against real production data. |

## Out of scope (v1.0)

- Multi-agent fields, gradient-based coordination (that's `Pneuma` /
  `Plexus` in `core/life/`).
- Payment streaming and metering (that's `Haima`).
- Homeostatic regulation across agents (that's `Autonomic`).
- Event-sourced cross-agent memory (that's `Lago`).
- Knowledge graph + bookkeeping pipeline (that's `bstack` skills).

If your need lands here, the answer is: use Bellows for the agent harness
*and* Life crates as a separate dependency tree on top. They were
designed to be composable.
