# Running Bellows

Three ways to run a Bellows agent today, ordered from fastest to most
production-shaped: **CLI one-shot**, **HTTP server (local)**, **HTTP
server (publicly reachable from your phone)**.

All three paths use the `repo-scout` example. Replace it with your own
workflow once you have one.

## 0. Credentials

Bellows talks to Claude. It auto-resolves credentials from the
environment in this order:

1. `ANTHROPIC_API_KEY` (`sk-ant-api03-...`) — standard API key, sent as
   `x-api-key`.
2. `CLAUDE_CODE_OAUTH_TOKEN` (`sk-ant-oat01-...`) — Claude Code
   subscription token, sent as `Authorization: Bearer ...` plus the
   `anthropic-beta: oauth-2025-04-20` header.

Set whichever you have. If neither is set, Bellows falls back to
`MockProvider` (which echoes the input — fine for sanity-checking the
HTTP shape without burning API credits).

```bash
# API key (production):
export ANTHROPIC_API_KEY="sk-ant-api03-..."

# OR Claude Code subscription:
export CLAUDE_CODE_OAUTH_TOKEN="sk-ant-oat01-..."
```

## 1. CLI one-shot — fastest

For development, CI, scripted invocations:

```bash
cd ~/broomva/core/bellows
cargo run --release -p bellows-example-repo-scout
```

This prints a single JSON object to stdout. Customise the input via
environment variables:

```bash
BELLOWS_MODEL="claude-haiku-4-5" \
BELLOWS_START_PATH="crates/bellows-core" \
BELLOWS_QUESTION="What traits does this crate define?" \
cargo run --release -p bellows-example-repo-scout
```

`BELLOWS_MODEL` is optional; the default is `claude-sonnet-4-5`. Use
`claude-haiku-4-5` for cheap/fast iteration during development.

## 2. HTTP server — local browser

Same binary, server mode:

```bash
cd ~/broomva/core/bellows
BELLOWS_MODE=server cargo run --release -p bellows-example-repo-scout
```

You'll see something like:

```text
[bellows] provider: anthropic (oauth)
[bellows] model:    claude-sonnet-4-5
INFO bellows server listening — open http://0.0.0.0:3548 for the web UI
```

Open the routes in any browser:

| Path | What |
|---|---|
| `http://localhost:3548/` | **Phone-friendly web UI** — paste JSON input, hit Run, see the response with provider/turns/tokens stats |
| `http://localhost:3548/healthz` | Liveness probe (`{"status":"ok","service":"bellows"}`) |
| `http://localhost:3548/v1/agents` | Lists the mounted workflow names + endpoints |
| `POST http://localhost:3548/v1/agents/repo-scout` | Invoke the workflow with a JSON body |

Quick curl smoke-test:

```bash
curl -s http://localhost:3548/healthz
curl -s http://localhost:3548/v1/agents
curl -s -X POST http://localhost:3548/v1/agents/repo-scout \
  -H 'content-type: application/json' \
  -d '{"start_path":".","question":"What is this project?"}' | jq .
```

The web UI auto-fills a sensible example input; on a phone Safari /
Chrome you can tap the textarea, edit the JSON, and run.

## 3. Public URL — testing from your actual phone

The local server already binds `0.0.0.0:3548`, so anything that can
reach your Mac can hit it. Three escalating options:

### 3a. Same WiFi (zero setup)

Find your Mac's LAN IP:

```bash
ipconfig getifaddr en0   # WiFi
# or:
ipconfig getifaddr en1   # ethernet/thunderbolt
```

On your phone (same WiFi), open `http://<that-ip>:3548/`. Plain HTTP,
no certificate, but works inside your network.

### 3b. Cloudflared quick tunnel — public HTTPS, no account

The fastest way to get a real HTTPS URL anywhere on the internet:

```bash
# Terminal 1 — keep the server up
BELLOWS_MODE=server cargo run --release -p bellows-example-repo-scout

# Terminal 2 — open the tunnel
cloudflared tunnel --url http://localhost:3548
```

Cloudflared prints a URL like
`https://birthday-reprint-arthritis-nashville.trycloudflare.com`. Open
it on your phone — that's the same UI as `localhost:3548/`, served
over HTTPS via Cloudflare's edge. No auth, no signup, ephemeral
(dies when you Ctrl-C the tunnel).

> **Security note:** the tunnel exposes the server with no auth. Anyone
> who guesses or sees the URL can run agents and burn your Claude
> credits. Treat tunnel URLs like passwords. For long-lived public
> deployments, add an auth layer (header check / JWT / Cloudflare
> Access) — currently a v0.3 roadmap item.

Cloudflared is `brew install cloudflared` if you don't have it.

### 3c. Tailscale — private mesh (most secure)

If you're already on Tailscale across your devices:

```bash
# Pin the server to your Tailscale IP if you want, or leave 0.0.0.0
BELLOWS_MODE=server cargo run --release -p bellows-example-repo-scout

# On the phone, install Tailscale, log into the same tailnet,
# then open: http://<this-mac-tailscale-name>:3548/
# e.g. http://broomva-mbp.tail-scale.ts.net:3548/
```

For public-internet reachability through Tailscale (not just the
mesh), use `tailscale funnel 3548` — that exposes port 3548 on a
public `*.ts.net` HTTPS URL backed by Tailscale auth.

### 3d. ngrok — alternative to cloudflared

If you prefer ngrok:

```bash
ngrok http 3548
```

Same idea, different vendor. cloudflared is recommended because it's
free for unlimited usage, no rate-limit on the free tier, and
supports the `quic` protocol for lower-latency tool-use rounds.

## What the web UI shows

After you POST an input, the UI surfaces:

- **HTTP status** + **latency** + **bytes**
- **provider** (`anthropic` / `mock`)
- **tokens** (input / output)
- **turns** (number of model calls in the autonomous loop)
- **session id** (ULID prefix, expand the JSON for the full id)
- The full JSON response, pretty-printed

This is enough to debug an agent end-to-end on a phone in under five
seconds.

## Hot-reload during development

For Rust development, use `cargo watch`:

```bash
cargo install cargo-watch  # one-time
cargo watch -x 'run --release -p bellows-example-repo-scout' \
  -- BELLOWS_MODE=server
```

Each save rebuilds + restarts the server. The web UI in the browser
reloads on its own when you hit Run again.

## Multi-workflow servers

The current `Server::new(workflow)` mounts exactly one workflow. v0.2
will add `Server::with_agent(workflow)` for multi-tenant servers — for
now, run one server per agent and front them with a router (Caddy /
Traefik / nginx) if you need them on one host.

## Operational notes

- **Sessions are in-memory.** A restart loses all session history. The
  workflow's *output* is durable (your caller persists it); only
  cross-call session continuity is lost. SQLite/Postgres backends land
  in v0.2.
- **No streaming yet.** The endpoint returns the full JSON when the
  agent finishes. For long-running agents (10+ second responses), the
  HTTP request will hang until done — set your client timeout
  accordingly. SSE streaming on `/v1/agents/{name}/stream` is the
  v0.2 plan.
- **Hooks fire identically over HTTP.** Whatever lifecycle hooks you
  registered on the engine fire for every HTTP request — including
  audit, deny, and stub semantics. The `repo-scout` example registers
  four hooks (TracingHook, CountingHook, PathPolicyHook,
  AllowDenyHook); their counters are part of every JSON response.
