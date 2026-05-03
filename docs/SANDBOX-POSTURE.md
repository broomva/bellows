# Sandbox Posture

> Honest threat model for Bellows sandboxes.
> Last updated: 2026-05-03.

Bellows sandboxes vary widely in their isolation guarantees. This file
documents what each impl does and does not enforce, so users can pick
the right tool for their threat model.

The framework does not enforce a single sandbox choice — it asks each
implementation to be honest in this document and in its docstrings.
"Honesty" here means: never claim more isolation than the impl
actually delivers.

## Trust model

Bellows assumes:

- The **agent author** trusts the binary they ship.
- The **caller** sending an `Input` to the agent may be untrusted.
- The **LLM provider** may emit harmful tool calls (intentional or
  accidental, prompt-injected, hallucinated). This is the sandbox's
  job to contain.
- The **content the agent reads** (issue bodies, files, MCP results)
  may contain prompt injection. The sandbox does not stop prompt
  injection — that is a model + system-prompt + filtering concern.

## Posture per implementation

### `bellows-sandbox` :: `VirtualSandbox`

**What it does:** in-process filesystem ops; no `exec`.

**Isolation:** none. `read`/`write`/`list` operate against the host
filesystem with no jail. `exec` returns an error.

**When to use:** tests, examples that don't shell out, in-process
deterministic compute.

**When NOT to use:** anything that runs LLM-emitted commands.

---

### `bellows-sandbox-local` :: `LocalSandbox` (default)

**What it does:** spawns commands via `tokio::process::Command` with
cwd jail, env-clear + caller-supplied env, and a configurable timeout
that kills on overrun.

**Isolation:** weak.
- `cwd` is restricted (relative paths resolve under workspace root).
- `env` defaults to empty + `PATH` only.
- A per-call timeout terminates the process.
- The agent runs as **the same user** as the parent process. It can
  read `~/.ssh`, write `/etc` if root, install software, exfiltrate
  any environment variable it can guess.

**When to use:** developer-trust scenarios. Same posture as `cargo`,
`make`, `npm`, or Claude Code itself. The user trusts the binary; the
binary runs the model; the model runs commands.

**When NOT to use:** running LLM-generated code from untrusted callers,
multi-tenant servers, anything internet-facing without an authenticated
front door.

---

### `bellows-sandbox-docker` :: `DockerSandbox` (v0.2, feature `sandbox-docker`)

**What it does:** runs commands inside a long-lived container per
session, via `bollard` to the local Docker / podman / OrbStack daemon.

**Isolation:** real.
- Container namespaces (mount, pid, net, user — depending on rootless).
- cgroups for CPU / memory / I/O limits.
- Network policy (default: no network unless `--with-network`).
- File mounts limited to caller-declared paths.

**When to use:** the agent is exposed to a multi-tenant input stream
(public issues, support tickets, customer code).

**When NOT to use:** environments without a Docker / podman daemon.

---

### `bellows-sandbox-namespaces` :: `NamespaceSandbox` (v0.2, Linux-only, feature `sandbox-namespaces`)

**What it does:** spawns commands inside a fresh user / mount / pid /
net namespace, with `landlock` filesystem restrictions and `seccompiler`
syscall filters.

**Isolation:** real, lighter than containers.
- Mount namespace + Landlock for FS path restriction.
- Seccomp filter to deny syscalls the agent doesn't need.
- User namespace for uid mapping (no root in host context).
- No daemon required.

**When to use:** Linux servers without Docker, want lightweight per-call
isolation, willing to invest in syscall-allowlist tuning.

**When NOT to use:** macOS, Windows, hardened Linux distros that disable
user namespaces.

---

### `bellows-sandbox-e2b` :: `E2BSandbox` (v0.2, feature `sandbox-e2b`)

**What it does:** HTTP client that delegates exec / fs ops to E2B's
Firecracker-microVM API.

**Isolation:** strongest practical.
- Vendor-managed microVM per session.
- Network policy controlled by E2B.
- Compute and storage limits per E2B plan.
- No host filesystem ever exposed.

**When to use:** running LLM-generated code, multi-tenant SaaS,
anything where a hostile model could be a problem.

**When NOT to use:** offline environments, latency-sensitive workloads
(network RTT per tool call), regulated environments where data residency
forbids third-party VMs.

---

## Which one is the default and why?

**`bellows-sandbox-local`** is the default. The reasoning:

1. Bellows' primary use case (today) is developer harnesses — the
   thing you build to triage your own issues, do code review on your own
   PRs, run your own data pipelines. In all of these, the user already
   trusts the binary.
2. Setting up Docker / namespaces / E2B is friction the framework
   should not impose by default.
3. Honesty is the safety story. The default's docstring and this file
   spell out what it does and does not protect.

Users who need real isolation flip the feature flag and swap the
sandbox at workflow construction:

```rust
let sandbox: Arc<dyn Sandbox> = Arc::new(bellows_sandbox_docker::DockerSandbox::new(...));
```

That's the entire migration cost.

## What the framework refuses to do

- **Claim isolation we don't enforce.** No "sandboxed" marketing on
  `bellows-sandbox-local`.
- **Default to the heavyweight option.** That would punish 95% of
  users for the 5% that need real isolation.
- **Hide which sandbox is active.** Every span carries the sandbox name,
  every audit log entry records it, the server's `/healthz` reports it.

## Future work

- A `Sandbox::posture()` method that returns a structured "what this
  enforces" descriptor. Lets the runtime warn at startup if a workflow
  is about to use `LocalSandbox` against an internet-facing route.
- A "sandbox compatibility test suite" that any new connector must pass
  — read/write a file, run a command, hit a timeout, fail closed on
  out-of-jail paths, etc.
- Integration with the `approval system` (v0.3) — a per-tool policy
  layer between the model and the sandbox.
