# Glossary

> What we mean by *workflow*, *step*, *role*, *sandbox*.

Every term below is used in Bellows source, docs, and commit messages
with the meaning fixed here. When in doubt, this file wins.

## Agent

The top-level concept. An agent is what a user *thinks* they're
building. In Bellows, an agent is realized as a `Workflow` plus a
configured runtime that drives it. "Agent" never appears in the trait
hierarchy because it would be ambiguous — `Workflow` is the precise term.

## Workflow

The user-implemented type that defines an agent. Provides:
- input / output types,
- a default `Role` (workflow-scoped, lowest precedence),
- the `SkillSet`, `ToolRegistry`, `Sandbox`, `ModelProvider` it uses,
- a deterministic `execute(ctx, input)` body.

Workflows compose: `ctx.subagent(other_workflow, input)` spawns a child
workflow with its own session.

## Step

One *autonomous-step boundary* inside a `Workflow::execute` body.
Calling `ctx.step(some_step, input).await` opens an inner loop in
`bellows-runtime`:

1. build `ModelRequest` (history + role + tools)
2. `ModelProvider::stream`
3. dispatch any tool calls
4. if `StopReason::ToolUse` → goto 1; else return

A `Step` is the only place model non-determinism enters. Code outside a
step is deterministic.

## Session

The persistent message history for one workflow invocation. Contains:
- a `SessionId` (ULID, time-ordered),
- ordered `Vec<Message>`,
- an optional session-scoped `Role` (mid-precedence),
- `serde_json::Map` metadata.

Sessions persist across an invocation via `SessionStore`. Sessions are
**not** shared between subagents — each subagent gets a fresh session.

## Role

A *non-persistent* identity-and-instructions overlay. Three scopes
(`Call`, `Session`, `Agent`) merge with precedence `call > session > agent`
via `Role::merge`. The merged role is rendered into the system-prompt
slot at request-build time and **never** inserted into `Session.history`.

This is what lets you swap a session's role without recomputing history,
and what lets per-call overrides happen without polluting the session
record.

## Sandbox

The connector trait for tool execution environments. Implementations
range from "no isolation, runs as you" (`bellows-sandbox-local`) to
"vendor-managed microVM" (`bellows-sandbox-e2b`). The `Sandbox` trait
exposes `exec`, `read`, `write`, `list` and is honest about the isolation
posture in its docstring.

The default is opinionated: subprocess + cwd + env-allowlist, same
posture as `cargo` or `make`. See `docs/SANDBOX-POSTURE.md`.

## Tool

A capability the model can invoke during a step. Has a `ToolSchema`
(name + description + JSON Schema for arguments) and an async `invoke`
that runs against a borrowed `Sandbox`. Tools do not own sandboxes —
the runtime injects one at call time.

Built-in tools live in `bellows-tool`: `BashTool`, `FsReadTool`,
`FsWriteTool`, `FsListTool`. MCP tools are exposed via the `McpTool`
adapter (v0.2, behind the `mcp` feature) — one entry per remote MCP tool.

## Skill

A reusable prompt fragment defined in Markdown with YAML frontmatter.
Same shape as Anthropic Skills, Flue skills, Broomva's `skills/`. Skills
are loaded once at workflow construction (embedded via `include_dir!`
in production, hot-reloaded via `load_dir` in `bellows dev`) and looked
up by name during execution.

A skill is *content*, not *code*. The workflow decides where the body
goes (system prompt, user message, tool result).

## Model provider

The vendor-neutral LLM abstraction. Implementations: Anthropic, OpenAI,
OpenRouter (v0.2), local via Ollama (v0.4). The trait is intentionally
narrow — `complete` and `stream` — so adding a provider is a focused
amount of work.

## Engine

The runtime concrete that drives a `Workflow` end-to-end:
constructs the `StepCtx`, runs `Workflow::execute`, persists the
session. Lives in `bellows-runtime`. Not user-facing — `bellows-server`
and `bellows-cli` wrap it.

## Server / CLI

Two ways to wrap an `Engine`. The server is `axum` + a workflow,
exposing `/v1/agents/:name`. The CLI is `bellows run` (one-shot, prints
JSON) or `bellows build` (codegen + cargo, produces a binary). Both
share the same `Engine`.

## Subagent

A child workflow invoked from a parent workflow. Spawned via
`ctx.subagent(workflow, input).await`. Gets its own fresh `Session` and
its own fresh tool registry — does not see the parent's history. The
parent receives the subagent's `Output` once it completes.

## Approval system (v0.3)

A policy layer between the model's tool calls and `Tool::invoke`. Lets
the runtime auto-approve some patterns, prompt for others, deny the
rest. Modeled after Codex's layered approval. Out of scope until v0.3.

## RF-* flags

In `docs/DEPENDENCY-CHAIN.md`, "RF-1", "RF-2", etc. are *open research
flags* — questions we noted at design time and have not yet resolved.
Resolve them by writing a spike + updating that document.
