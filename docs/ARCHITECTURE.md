# Architecture

> The system, the trait set, the control flow, and the seams.
> Last updated: 2026-05-03.

## 1. System view

Bellows is a layered framework. Each layer is a crate; each crate has a
single responsibility. The contract crate (`bellows-core`) sits at the
bottom and is depended on by everything; nothing depends back on
implementation crates.

```
                      ┌──────────────────────────────────┐
                      │            bellows-cli           │  binary
                      └────────────┬─────────────────────┘
                                   │
                      ┌────────────▼─────────────────────┐
                      │          bellows-server          │  axum + workflow → HTTP
                      └────────────┬─────────────────────┘
                                   │
                      ┌────────────▼─────────────────────┐
                      │         bellows-runtime          │  workflow engine
                      └─┬──────┬──────┬─────────┬────────┘
                        │      │      │         │
                  ┌─────▼┐ ┌──▼──┐ ┌──▼───┐ ┌───▼──────┐
                  │skill │ │tool │ │model │ │ session  │
                  └──┬───┘ └──┬──┘ └──┬───┘ └────┬─────┘
                     │        │       │          │
                     │   ┌────▼───────▼────┐     │
                     │   │  sandbox-local  │     │      (or sandbox-docker /
                     │   │  + sandbox      │     │       sandbox-e2b /
                     │   └────────┬────────┘     │       sandbox-namespaces)
                     │            │              │
                     └────────┬───┴────────┬─────┘
                              │            │
                       ┌──────▼────────────▼──────┐
                       │       bellows-core       │ contract — types + traits
                       └──────────────────────────┘
```

## 2. Conceptual flow

A single workflow invocation:

```text
1. Caller produces an Input value (typed, JSON-deserializable).
2. Engine::run                              ←— bellows-runtime
   ├─ create or load Session                ←— bellows-session
   ├─ assemble StepCtx (model, tools, sandbox, skills, span)
   └─ call Workflow::execute(ctx, input)    ←— user code
       │
       └─ ctx.step(SomeStep).await?         ←— autonomous boundary
           │
           └─ inner loop:                   ←— bellows-runtime
              ├─ Role::merge(agent, session, call)
              ├─ build ModelRequest
              ├─ ModelProvider::stream      ←— bellows-model
              ├─ tool calls? → invoke via ToolRegistry
              │   └─ Tool::invoke(args, sandbox)
              │       └─ Sandbox::exec / read / write / list
              ├─ append ToolResults to Session
              └─ if StopReason::ToolUse: loop; else: return
3. Engine persists the session via SessionStore.
4. Caller receives the typed Output.
```

The deterministic outer code is `Workflow::execute` and the contents of
your Rust source. The non-deterministic part is *strictly contained
inside `Step::run`*. This is the fundamental architectural assertion of
Bellows; preserving it is what makes agents replayable.

## 3. The trait set (kernel contract)

These are the only types and traits a user of Bellows ever needs to know.
They live in `bellows-core` and have no implementation dependencies.

| Trait / Type | Role |
|---|---|
| `Workflow` | The user implements this. Declares input/output, role, skills, tools, sandbox, model, and an `execute` body. |
| `Step` | One autonomous-step boundary inside a workflow. Has `Input`, `Output`, `name`, and `run`. |
| `StepCtx` | Per-call context handed into steps: session, model, tools, sandbox, skills, tracing span. |
| `Session` | Persistent message history + meta. Owned by the runtime; mutated through `StepCtx`. |
| `SessionStore` | Trait for session persistence (`MemoryStore`, future `SqliteStore`, `PostgresStore`). |
| `Sandbox` | Connector trait. `name`, `exec`, `read`, `write`, `list`. |
| `Tool` | One callable from the model. `schema()` describes args; `invoke()` runs it (with a `Sandbox` reference). |
| `ToolRegistry` | Resolves names to `Tool`s. |
| `Skill`, `SkillSet` | Markdown frontmatter+body, looked up by name. `EmptySkillSet` is the default. |
| `ModelProvider` | LLM abstraction. `id`, `complete`, `stream`. |
| `ModelRequest` | Built fresh per turn from session + role + tools. |
| `ModelResponse` / `ModelStreamEvent` | Output shapes. |
| `Role`, `RoleScope` | System-prompt overlay. **Not persisted.** Merged via `Role::merge(agent, session, call)`. |
| `Message`, `MsgRole`, `ToolCall`, `ToolResult` | Conversation history primitives. |
| `BellowsError`, `Result<T>` | The single error type returned across every contract boundary. |

## 4. Decision points (where the seams matter)

These are the four places a contributor must understand before
proposing structural changes.

### 4.1 Tools vs. Sandbox

`Tool::invoke` takes `&dyn Sandbox` as a parameter, not a field. Tools
do not own sandboxes. The runtime injects one at call time. This means
swapping `local` for `docker` requires zero tool changes, and tools that
don't need a sandbox (e.g. an MCP-mediated remote tool, a pure compute
tool) simply ignore the argument. This boundary is non-negotiable.

### 4.2 Where the autonomous loop lives

The model + tool loop runs in `bellows-runtime`, dispatched when a user
calls `ctx.step(...)`. It does **not** run in `Workflow::execute`. This
means:

- `Workflow::execute` reads as deterministic Rust.
- Replay tests can mock `ModelProvider` and verify orchestration.
- The autonomous code path is in *one place* and improvements to it
  (parallel tool execution, streaming, retries, compaction) benefit
  every workflow without touching workflow code.

### 4.3 Role precedence — kernel concern

`Role::merge(agent, session, call)` is the **only** place precedence is
computed. Implementations must not roll their own merge. The merged role
is applied at request-build time inside `bellows-runtime` and never
inserted into `Session.history`. This is what keeps history clean across
role swaps, and what lets a session be replayed under a different role
without recomputing history.

### 4.4 MCP — adapter, not first-class

MCP servers are exposed via a single `McpTool` adapter inside
`bellows-tool` (v0.2 behind the `mcp` feature). Each remote MCP tool
becomes one entry in the unified `ToolRegistry`. This means the runtime
sees one mental model for tools — local or remote, built-in or MCP —
and observability traces are uniform.

### 4.5 Session storage — trait in core, impls in `bellows-session`

`SessionStore` lives in `bellows-core` (the contract); `MemoryStore`,
`SqliteStore`, `PostgresStore` live in `bellows-session` (feature-gated).
This mirrors the convention `praxis` uses against `aios-protocol` in the
broader Broomva ecosystem.

## 5. Build / deploy story

`bellows build` (lands in v0.2 via `bellows-build`) works in three phases:

1. **Discover.** Read the user's `Cargo.toml`, find workflow types
   tagged with `#[workflow]` (registered via the `inventory` pattern),
   walk `./skills/**/*.md` and `./roles/*.md`.

2. **Codegen a thin wrapper crate.** Write `target/bellows-build/<name>/`
   containing a generated `main.rs` that wires the user's workflow into
   `bellows-server::serve`. Skills are embedded with `include_dir!` —
   compile-time, zero runtime IO. Roles likewise. Production artifacts
   are hermetic.

3. **Cargo build.** Shell out to `cargo build --release`. Use
   `cargo_metadata` for read-only introspection. Output binary copied to
   `dist/<name>` plus a `dist/manifest.json` describing routes, version,
   build hash.

`bellows run` (one-shot CLI mode) uses the same codegen path but invokes
the binary with `--once --input <json>`, prints output to stdout, and
exits. CI-friendly.

## 6. Observability

Every `Step::run` and `Tool::invoke` opens a `tracing::Span`. Spans
carry: workflow name, session id, step name, tool name, model id. The
runtime exports OTLP via `tracing-opentelemetry` when `OTEL_EXPORTER_OTLP_ENDPOINT`
is set in the environment. No vendor lock-in.

The session itself is the audit log: every model turn and every tool
result lands in `Session.history`. `SessionStore` implementations are
free to also stream to external systems (e.g. Lago, S3, Postgres) —
that's a non-contract concern.

## 7. What we explicitly do not do (in v0.1)

- **No multi-agent fields.** Pneuma / Plexus is a Life concern. Bellows
  agents are independent units.
- **No payment substrate.** Haima is a Life concern.
- **No homeostasis controller.** Autonomic is a Life concern.
- **No event-sourced persistence.** `Session` is enough for one
  invocation; long-horizon memory belongs to the storage backend.
- **No formal RCS hierarchy.** Bellows operates at L0 (the harness) only.

These are intentional limits. They are what keeps Bellows a developer
harness rather than another agent OS.
