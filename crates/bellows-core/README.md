# bellows-core

Canonical kernel contract for the [Bellows](../../README.md) agent-harness framework.

This crate defines **types and traits only** — no logic, no I/O, no runtime
dependencies. Every other crate in the workspace depends on `bellows-core`;
`bellows-core` depends on nothing in the workspace. This is the same convention
used by `aios-protocol` in the broader Broomva ecosystem.

## What lives here

| Concept | Item |
|---|---|
| Agent contract | `Workflow` trait + `Step` trait |
| Execution context | `StepCtx` struct |
| Conversation | `Session` struct, `SessionStore` trait, `Message`, `MsgRole`, `ToolCall` |
| Identity overlay | `Role` struct, `RoleScope`, `Role::merge` |
| Tool surface | `Tool` trait, `ToolRegistry` trait |
| Sandbox surface | `Sandbox` trait, `ExecOpts`, `ExecResult`, `DirEntry` |
| Skill surface | `Skill` struct, `SkillSet` trait |
| Model surface | `ModelProvider` trait, `ModelRequest`, `ModelResponse`, `ModelStream` |
| Errors | `BellowsError`, `Result<T>` alias |

## What does NOT live here

- Implementations of any trait. Implementations live in the focused crates
  (`bellows-skill`, `bellows-runtime`, `bellows-sandbox-local`, etc.).
- HTTP, async runtime initialization, or any I/O.
- Markdown parsing, MCP wire format, or vendor-specific model APIs.

## Stability

`bellows-core` is the most stable surface in the workspace. Breaking changes
require a major version bump and a written migration note in `CHANGELOG.md`.
