# AGENTS.md — Bellows

> Operational rules for autonomous and human agents working in this repo.

## Scope

This file documents the **operational layer** of the workspace: what to
run, what to gate, how to land a change. The **invariant layer** lives in
[CLAUDE.md](CLAUDE.md). The **control-systems metalayer** lives in
[METALAYER.md](METALAYER.md).

## Workflow boundaries

Every change should fit one of three buckets:

| Bucket | What | Gate |
|---|---|---|
| **Contract** | edits `bellows-core` traits/types | requires major-version bump if breaking; design doc reviewed; CHANGELOG entry |
| **Implementation** | edits a non-core crate body | unit tests added/updated; clippy clean; no widening of public API without note |
| **Infrastructure** | governance files, CI, docs, schemas | docs must stay coherent; cross-references kept in sync |

## Working rules

1. **Read the kernel contract first.** Any change to a non-core crate
   should start by checking what `bellows-core` already exposes. Most
   changes should not require a contract change.

2. **Local checks before push.**
   ```bash
   cargo check --workspace
   cargo test  --workspace --all-features
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   ```

3. **Conversation history is context.** Prior session logs are bridged
   into `docs/conversations/` (once `knowledge-graph-memory` is wired
   here). Scan them before re-solving a problem.

4. **No silent error swallowing.** Errors must propagate as
   `BellowsError::*` or be logged with `tracing::warn!` / `tracing::error!`
   at the seam where they are absorbed.

5. **Sandbox claims must be true.** When you ship a sandbox connector
   with isolation properties, the docstring on the impl must specify
   what is and is not enforced. We grade ourselves on `docs/SANDBOX-POSTURE.md`.

6. **No Life dependency.** Bellows compiles against an empty Life
   workspace by design.

## Gates (control metalayer)

The control kernel installer dropped baseline gate definitions in
[METALAYER.md](METALAYER.md). For the v0.1 → v0.2 cadence, the active
gates are:

| Gate | Trigger | Blocks |
|---|---|---|
| **G1: Build** | every push | `cargo check --workspace` must pass |
| **G2: Test** | every PR | `cargo test --workspace` must pass |
| **G3: Lint** | every PR | `cargo clippy -- -D warnings` must pass |
| **G4: Contract delta** | edits to `bellows-core` | requires CHANGELOG entry + reviewer |
| **G5: License** | every PR with new deps | `cargo deny check licenses` must pass |
| **G6: Doc coherence** | edits to ARCHITECTURE.md / DEPENDENCY-CHAIN.md | cross-refs verified manually |

Future gates (post-v0.2): supply-chain audit (`cargo deny check advisories`),
binary-size budget on `bellows-cli`, MSRV check via `cargo msrv`.

## Branch conventions

- `main` is always green.
- Feature branches: `feat/<short-slug>` or `fix/<short-slug>`.
- One PR per concept. Squash-merge with a Conventional Commit message.

## Asking for review

Open the PR with:

1. **What changed** — one paragraph.
2. **Why** — link to the doc, ticket, or conversation that motivated it.
3. **Risk** — what breaks if this is wrong, and what's the rollback.
4. **Tests** — the cases the change covers.

For contract changes (`bellows-core`), include a *migration note* even
if the change is technically backward-compatible — future-you will thank
present-you.

## Releasing

v0.x releases: tag, publish each crate to crates.io in dependency order
(`bellows-core` first, then siblings, then `bellows-cli` last). Use
`cargo publish --dry-run` against every crate first.

v1.0 requires:
- The full `Step::run` autonomous loop wired (currently scaffolded only)
- Anthropic + OpenAI providers shipping in `bellows-model`
- `bellows build` producing real `dist/<name>/bellows-<name>` artifacts
- `bellows-sandbox-docker` shipped at minimum
- 100% workspace clippy clean with `pedantic` + `nursery`
- The Roadmap's v1.0 checklist all green
