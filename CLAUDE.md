# CLAUDE.md — Bellows

> Instructions for AI agents working in this repository.
> Last updated: 2026-05-03.

## What this repo is

**Bellows** is a Rust open-source agent-harness framework. It is the
clean-room Rust expression of the architectural primitives that
[Flue](https://flueframework.com/) (Fred K. Schott, withastro org,
Apache-2.0) defines for TypeScript.

This repo is **separate from `core/life/`**. Bellows takes **no
dependencies** on Life crates (`arcan`, `praxis`, `lago`, `anima`,
`autonomic`, etc). The two are siblings under `core/` that share Rust
conventions but no code.

## Invariants (do not violate)

1. **`bellows-core` has no implementation dependencies.** It depends on
   `serde`, `async-trait`, `thiserror`, `ulid`, `tracing`, and `futures`.
   Nothing else. No I/O, no HTTP, no async-runtime initialization, no
   Markdown parsing.

2. **No Life dependencies, ever.** Bellows must compile against an
   empty `~/broomva/core/life/`. If you need a feature that exists in a
   Life crate, copy the *idea* not the code.

3. **`Role` overlays are never persisted.** They are applied at
   `ModelRequest` build time. Inserting a role into `Session.history`
   is a bug — the role-merge precedence (`call > session > agent`)
   becomes meaningless if roles get baked into history.

4. **The deterministic / autonomous boundary lives in `Step`.** Code
   above `ctx.step(...).await` is deterministic; code inside `Step::run`
   is the only place the model+tool loop runs. Workflow tests should
   assert this by mocking `ModelProvider` and verifying that `execute`
   produces identical traces with identical inputs.

5. **Sandbox honesty.** The default `bellows-sandbox-local` provides
   subprocess isolation only — no namespaces, no containers. Docs must
   say so out loud. Never claim isolation we don't actually provide.

## How to extend things

| Adding... | Lives in | Tests in |
|---|---|---|
| A new tool | `bellows-tool` (built-in) or a downstream crate | unit tests in the tool crate |
| A new sandbox | `bellows-sandbox-<name>` (sibling crate) | unit + integration tests in that crate |
| A new model provider | `bellows-model` (or a feature-gated module of it) | wiremock-based contract tests |
| A new skill format | `bellows-skill` | snapshot tests via `insta` |
| A new server route | `bellows-server` | axum integration tests |
| A breaking change to `bellows-core` | requires a major version bump + CHANGELOG entry |

## Conventions

- **Rust 2024 edition, MSRV 1.85.** Pinned in `rust-toolchain.toml`.
- **No `unwrap()`, no `expect()`, no `panic!()`** in non-test code. Workspace
  `clippy` lints deny these. Use `BellowsError::*` variants.
- **No emojis in source files** (LLM-friendly diffs, no Unicode surprises).
- **Use `tracing::info_span!` everywhere a span boundary is conceptually
  meaningful.** Especially `Step::run` and `Tool::invoke`.
- **`async_trait` only.** When `async fn in trait` becomes stable enough
  for our MSRV, we'll migrate workspace-wide.
- **Snapshot tests via `insta`** for skill parsing, model-request
  rendering, and tool schemas — outputs that need stability.
- **Workspace lints are mandatory.** `pedantic`, `nursery`, `cargo`
  warned; `unwrap_used`, `expect_used`, `panic`, `dbg_macro` denied.

## Commit conventions

- **One concept per commit.** No drive-by formatting in feature commits.
- **Conventional Commits** prefix: `feat(scope):`, `fix(scope):`, `docs:`, `refactor:`, `test:`, `chore:`.
- **Reference the affected crate** in the scope: `feat(runtime): wire role merge`.

## When you need to consult prior decisions

- **Why is X this way?** → `docs/ARCHITECTURE.md` and the `docs/conversations/`
  bridge (when wired). The repo's design is intentionally documented.
- **Why this dependency?** → `docs/DEPENDENCY-CHAIN.md`.
- **Why no Life dep?** → top of this file (invariant 2) and `README.md`.
- **What does v0.2 add?** → `docs/ROADMAP.md`.

## Don't

- Do not add a Life crate dependency.
- Do not introduce a runtime dep that isn't already in `Cargo.toml`'s
  `[workspace.dependencies]` without updating `docs/DEPENDENCY-CHAIN.md`
  and adding to `deny.toml` if needed.
- Do not bypass the role-merge precedence rule (`call > session > agent`).
- Do not put logic in `bellows-core`. Contract crate, contract only.
- Do not silently widen the threat model. Sandbox honesty is a value.

## Useful commands

```bash
cargo check --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
```
