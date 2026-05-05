# Bellows

[![CI](https://github.com/broomva/bellows/actions/workflows/ci.yml/badge.svg)](https://github.com/broomva/bellows/actions/workflows/ci.yml)

> *Pump work into the agent. Catch the heat in the harness.*

**Bellows** is a Rust open-source agent-harness framework — a clean-room
re-implementation of the architectural primitives that
[Flue](https://flueframework.com/) defines for TypeScript: deterministic
workflows orchestrating autonomous LLM steps, pluggable sandboxes,
Markdown-defined skills, role-precedence overlays, and an
HTTP-server compile target.

Where Flue says **Agent = Model + Harness**, Bellows says it in Rust.

> **Status: 0.1.0-pre — kernel contract published, runtime is scaffolding.**
> The trait set in `bellows-core` is the design surface library users will build
> against. The runtime, server, and CLI bodies are functional but minimal —
> see [docs/ROADMAP.md](docs/ROADMAP.md) for what lands when.

## Why does Bellows exist?

Three observations make this worth building:

1. **The harness is the hard part.** Claude Code's source map leak
   (March 2026) and OpenAI Codex's open-source CLI both confirmed it: the
   model is the easy part, the harness around it (context management, tool
   dispatch, approval system, replay, sandbox) is where the engineering
   actually lives.
2. **No Rust harness fills this niche.** `rig` is a Rust SDK. `swiftide`
   is for RAG. Mastra and Vercel AI SDK 6 own the TypeScript side. There
   is no Rust framework that ports Flue's deterministic-workflow +
   autonomous-step + multi-sandbox + skills-as-Markdown shape.
3. **Rust is the right substrate when you need it.** Single-binary
   deploys, predictable memory, no npm-supply-chain surface, real
   concurrency, and a type system that catches role-precedence
   bugs at compile time.

Bellows is the Rust harness for teams that want their agent stack to
ship as one auditable binary.

## Mental model

```text
┌── Workflow::execute (deterministic — your code) ────────────────────────┐
│                                                                         │
│   ctx.step(Fetch).await?      ←── autonomous: model + tools + sandbox   │
│   ctx.step(Classify).await?   ←── autonomous: model + tools + sandbox   │
│   ctx.subagent(Other).await?  ←── isolated child workflow               │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

The deterministic outer orchestration is what makes Bellows agents
**replayable** and **testable**. The non-determinism is contained inside
each `Step` — and even there, the model + tool loop is auditable via
`tracing` spans and the persisted `Session` history.

## Workspace layout

| Crate                    | Purpose |
|--------------------------|---------|
| `bellows-core`           | Canonical kernel contract — types and traits, no logic. |
| `bellows-skill`          | Markdown skill loader (frontmatter + body parser). |
| `bellows-session`        | Session storage — `MemoryStore` ships now; SQLite/Postgres in v0.2. |
| `bellows-model`          | LLM provider abstraction — `MockProvider` ships now; Anthropic/OpenAI/OpenRouter in v0.2. |
| `bellows-sandbox`        | Sandbox connector trait + `VirtualSandbox` (in-process). |
| `bellows-sandbox-local`  | Subprocess sandbox with cwd jail, env allowlist, per-call timeout. |
| `bellows-tool`           | Built-in tools: `BashTool`, `FsReadTool`, `FsWriteTool`, `FsListTool`. |
| `bellows-runtime`        | Workflow engine that drives `Workflow::execute` end-to-end. |
| `bellows-server`         | Axum HTTP server wrapping a workflow into a deployable artifact. |
| `bellows-cli`            | The `bellows` binary. |
| `examples/issue-triage`  | End-to-end example workflow. |

In v0.2 we add `bellows-build` (codegen + cargo orchestration for
`bellows build`), `bellows-macros` (`#[workflow]`, `#[step]` proc-macros),
`bellows-sandbox-docker` (bollard), and `bellows-sandbox-e2b` (remote).

## Quick start

```bash
# In this workspace:
cargo run -p bellows-cli -- version
cargo run -p bellows-cli -- doctor

# Run the example agent (one-shot CLI mode):
cargo run -p bellows-example-issue-triage

# Or as an HTTP server on port 3548:
BELLOWS_MODE=server cargo run -p bellows-example-issue-triage
curl http://localhost:3548/healthz
```

## Documentation map

- **[docs/RUNNING.md](docs/RUNNING.md)** — how to run an agent (CLI, HTTP server, public URL via cloudflared / Tailscale, phone access).
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — full architecture, traits, control flow.
- **[docs/DEPENDENCY-CHAIN.md](docs/DEPENDENCY-CHAIN.md)** — every dependency, every layer, every justification.
- **[docs/ROADMAP.md](docs/ROADMAP.md)** — v0.1 → v1.0 plan with explicit gates.
- **[docs/GLOSSARY.md](docs/GLOSSARY.md)** — what we mean by *workflow*, *step*, *role*, *sandbox*.
- **[docs/SANDBOX-POSTURE.md](docs/SANDBOX-POSTURE.md)** — honest threat-model document.
- **[CLAUDE.md](CLAUDE.md)** — instructions for AI agents working in this repo.
- **[AGENTS.md](AGENTS.md)** — operational rules and harness commands.
- **[METALAYER.md](METALAYER.md)** — the control-systems metalayer governing this workspace.

## Relationship to the broader Broomva ecosystem

Bellows is **separate from `core/life/`**. It does not depend on `arcan`,
`praxis`, `lago`, `anima`, `autonomic`, or any other Life crate. It is a
sibling project that shares conventions (Rust 2024, MSRV 1.85, Apache-2.0,
single-responsibility crate boundaries, canonical kernel-contract
crate), but no code.

This separation is intentional. Life is an Agent OS economy; Bellows is a
developer harness framework. The first lets agents persist, regulate, pay
each other, and coordinate as fields. The second lets a developer write
"here is my agent, deploy it" in one Rust binary. Both are useful; the
boundaries don't help the user when blurred.

## License

Apache-2.0. See [LICENSE](LICENSE).

## Inspiration & credits

The architecture in this repo is a clean-room Rust expression of the
public design Fred K. Schott (Astro co-founder) and the `withastro/flue`
team published. Bellows shares no source with Flue. Where the Flue docs
named a primitive that mapped cleanly onto Rust idioms, we kept the name
(`Workflow`, `Step`, `Sandbox`, `Skill`, `Role`); where Rust idioms diverged
(stream types, error types, trait-object choices), we picked the Rust path.
