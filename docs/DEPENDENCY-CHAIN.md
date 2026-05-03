# Dependency Chain

> Every dependency, every layer, every justification.
> Last updated: 2026-05-03.

This document is the audit trail for why each dependency in
`Cargo.toml`'s `[workspace.dependencies]` was chosen. When a contributor
proposes a change, this is where the rationale gets updated.

## Conceptual dependency chain (top-down)

```
User code                      → Workflow trait              (bellows-core)
Workflow::execute              → StepCtx                     (bellows-core)
                               → Step::run                   (bellows-core)
                               → Sandbox / Tool / Model      (bellows-core)
StepCtx                        → ModelProvider, ToolRegistry, Sandbox, SkillSet
Engine                         → SessionStore                (bellows-session)
                               → all StepCtx components
Server                         → Engine                      (bellows-runtime)
                               → axum + tokio runtime
CLI                            → Engine + Server             (bellows-runtime, bellows-server)
```

## Layer-by-layer dependency table

| # | Layer | Primary | Version | License | Why | Alternative considered | Rejected because |
|---|-------|---------|---------|---------|-----|------------------------|------------------|
| 1 | Async runtime | `tokio` | 1.42 | MIT | Ecosystem standard. axum/reqwest/tonic all on tokio. Life monorepo standardizes on tokio. | `smol` / `async-std` | smol fragmenting; async-std unmaintained since 2022. |
| 2 | HTTP server | `axum` | 0.8 | MIT | tower-based, hyper 1.x, type-safe extractors, low transitive footprint. Matches Life's lifegw. | `actix-web`, `poem` | actix runtime split from tower; poem smaller community. |
| 3 | HTTP client | `reqwest` | 0.12 | MIT/Apache | rustls (no OpenSSL), streaming bodies for SSE, hyper 1.x. | `hyper` direct, `ureq` | hyper 10× more boilerplate for SSE; ureq sync-only. |
| 4 | CLI parsing | `clap` | 4.5 | MIT/Apache | Derive macros, subcommand UX matches cargo. | `argh`, `lexopt` | argh thin help; lexopt good for tiny binaries only. |
| 5 | Serialization (struct) | `serde` + `serde_json` | 1 | MIT/Apache | Universal. | none | n/a |
| 6 | Serialization (config) | `toml` | 0.8 | MIT/Apache | Used for `bellows.toml` agent config files. | `figment`, `config-rs` | adds runtime layer Bellows doesn't need yet. |
| 7 | Serialization (skill frontmatter) | `serde_yaml_ng` | 0.10 | MIT/Apache | Maintained fork; original `serde_yaml` archived 2024. | `yaml-rust2` | doesn't integrate with serde derive. |
| 8 | Error handling (libs) | `thiserror` | 2 | MIT/Apache | Typed errors at every contract boundary. v2.x = faster compile. | `snafu` | more verbose for marginal benefit. |
| 9 | Error handling (binary) | `anyhow` | 1 | MIT/Apache | Glue in `bellows-cli`. | `eyre` | smaller community. |
| 10 | Error UX (user-facing) | `miette` | 7 | MIT/Apache | Ariadne-style colored diagnostics for skill-parse errors — UX differentiator. | `codespan-reporting` | heavier, less ergonomic. |
| 11 | Tracing core | `tracing` | 0.1 | MIT/Apache | de-facto Rust standard, span instrumentation across async. | `log`, `slog` | `log` lacks structured spans; slog older API. |
| 12 | Tracing subscribers | `tracing-subscriber` | 0.3 | MIT/Apache | env-filter + json output. | none | n/a |
| 13 | OTLP export | `tracing-opentelemetry` + `opentelemetry-otlp` | 0.27 | Apache-2.0 | Drop-in compat with any OTel collector. | `console-subscriber` | tokio-internal only, not vendor-neutral. |
| 14 | Markdown | `pulldown-cmark` | 0.12 | MIT | CommonMark-compliant, zero-alloc streaming, no C deps. | `comrak`, `markdown-rs` | comrak depends on libcmark; markdown-rs heavier (mdast). |
| 15 | Frontmatter splitter | hand-rolled | n/a | n/a | 30 lines, lets us define typed frontmatter contracts. | `gray_matter`, `yaml-front-matter` | gray_matter pulls regex + extra deps; untyped HashMap. |
| 16 | Embedded files | `include_dir` | 0.7 | MIT | Compile-time skill embedding for hermetic artifacts. | `rust-embed` | proc-macro heavier; `include_dir` is the simpler shape. |
| 17 | LLM providers | **TBD — `genai` candidate** | 0.3 | MIT/Apache | Single trait, Anthropic/OpenAI/OpenRouter/Gemini/Groq/Ollama coverage. **Research flag: verify Anthropic prompt-caching headers + tool-use streaming.** | `rig`, per-provider SDKs, `llm` (rustformers) | rig reinvents what Bellows itself provides; per-provider = N abstractions; `llm` is local-inference only. |
| 18 | MCP | `rmcp` | 0.15 | MIT | Anthropic-blessed Rust SDK. Same as Praxis. | `mcp-rust-sdk` | older fork. |
| 19 | Sandbox abstraction | trait in `bellows-core` | n/a | n/a | Feature-gated impls in sibling crates. Default has zero sandbox extras. | one impl crate | bifurcates trait + impl awkwardly. |
| 20 | Sandbox: subprocess | `tokio::process::Command` | (via tokio) | MIT | Default. Honest about isolation = none. | `std::process` | not async. |
| 21 | Sandbox: Docker (v0.2) | `bollard` | 0.18 | Apache-2.0 | Mature Docker/podman client. | `containerd-client` | thinner; bollard covers 95% of needs. |
| 22 | Sandbox: namespaces (v0.2, Linux) | `nix` + `caps` + `landlock` + `seccompiler` | latest | MIT/Apache | Lightweight isolation without container daemon. | (none — direct syscalls only) | seccompiler is the right primitive. |
| 23 | Sandbox: remote (v0.2) | `reqwest` (E2B/Daytona client) | (via reqwest) | MIT/Apache | No first-party Rust SDK from these vendors as of 2026. | (none) | n/a |
| 24 | IDs | `ulid` | 1 | MIT | Lexicographically sortable, time-ordered for log/trace ergonomics. | `uuid` | unsorted, less ergonomic for time-series. |
| 25 | Hashing | `blake3` | 1 | Apache-2.0/MIT | 10× faster than sha2, content-addressing for cached responses. | `sha2` | slower; only matters if compatibility required. |
| 26 | Snapshot tests | `insta` | 1.40 | Apache-2.0 | Skill parse + tool schema + role render snapshots. | none | n/a |
| 27 | HTTP mock | `wiremock` | 0.6 | Apache-2.0 | Mock LLM provider HTTP for contract tests. | `mockito` | wiremock has more powerful matchers. |
| 28 | CLI tests | `assert_cmd` + `predicates` + `tempfile` | 2 / 3 / 3 | MIT/Apache | Full `bellows` CLI behavior tests. | none | n/a |
| 29 | Macro plumbing (v0.2 `bellows-macros`) | `syn`, `quote` | 2, 1 | MIT/Apache | Standard proc-macro stack. | none | n/a |
| 30 | Build orchestration (v0.2 `bellows-build`) | `cargo_metadata` + `tera` + `which` | latest | MIT/Apache | `cargo_metadata` for read-only introspection; `tera` for codegen templates; `which` for finding the cargo binary. | use `cargo` as a library | unstable API, huge dep footprint. |

## Transitive dependency budget

Approximate transitive crate count for the v0.1 default workspace
(no feature flags, no v0.2 crates):

| Component | Approx crates pulled |
|---|---|
| tokio (full) | ~40 |
| axum 0.8 + tower | ~80 |
| reqwest 0.12 (rustls + json + stream) | ~70 |
| serde + serde_json + serde_yaml_ng + toml | ~25 |
| tracing + subscriber + otel | ~50 |
| clap 4.5 | ~20 |
| pulldown-cmark + include_dir | ~10 |
| ulid + blake3 + thiserror + miette + futures + async-trait | ~30 |
| **Total v0.1 default** | **~325** |

That's roughly **3-4× smaller than a `rig`-based stack** (which pulls
its own provider modules, vector store, RAG glue) — by design. Bellows
intentionally does not own RAG, embeddings, or vector storage.

## Operational / infrastructure dependencies

Beyond runtime crates:

| Need | Provider | Alternative |
|---|---|---|
| CI | GitHub Actions (planned) | GitLab CI |
| Crate publish | crates.io | n/a |
| Container distribution (v0.2 `bellows build` artifacts) | OCI / Docker Hub via user-supplied `Dockerfile` | n/a |
| Observability backend | any OTLP-compatible collector (Honeycomb, Grafana Tempo, Datadog) | none required for local |
| Optional remote sandbox (v0.2) | E2B (`e2b.dev`) | Daytona, Modal |
| Optional MCP servers | any compliant MCP server | n/a |

## Documentation dependencies

This document depends on:
- `Cargo.toml` (`[workspace.dependencies]`) — the source of truth.
- `docs/ROADMAP.md` — for "v0.2" / "v1.0" tags.
- `docs/SANDBOX-POSTURE.md` — for sandbox claims.

When you add a workspace dep, update **all three** in the same commit.

## Open research flags

| # | Question | Resolution path |
|---|----------|-----------------|
| RF-1 | Does `genai` 0.3 expose Anthropic prompt-caching headers and tool-use streaming deltas? | spike a 200-line connector against Claude in a feature branch; if missing, fall back to hand-rolled per-provider modules. |
| RF-2 | Should v0.2 sandbox connectors include Firecracker microVMs (via `firepilot` / `rust-vmm`)? | re-evaluate after `bellows-sandbox-docker` ships and we have data on user demand. |
| RF-3 | Should `Session.meta` carry a typed `Provenance` field (provider, model, build hash) by default? | propose at v0.2 design review. |
| RF-4 | Is `tracing-opentelemetry` 0.27 wire-compatible with Life's `vigil` OTLP exporter? | check at v0.2 integration spike (optional Life-bridge feature). |
