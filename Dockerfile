# syntax=docker/dockerfile:1.7
# Bellows — multi-stage build for the repo-scout example agent.
# Targets a minimal runtime image (debian-slim, ~70 MB) with a statically-
# linked-ish Rust binary. CA certs are needed for HTTPS to api.anthropic.com.
#
# Customize which agent binary is shipped via the BIN build-arg. Default:
# repo-scout (the demo agent). To deploy a different agent: pass
# `--build-arg BIN=my-agent` at build time.

ARG BIN=repo-scout
ARG RUST_VERSION=1.85

# ── Builder ────────────────────────────────────────────────────────────────
FROM rust:${RUST_VERSION}-slim-bookworm AS builder
ARG BIN
WORKDIR /build

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Copy the workspace. Let Cargo handle dependency caching internally — the
# typical "copy Cargo.toml first to cache deps" trick doesn't compose well
# with workspace member discovery, and our dep tree is small enough that the
# full build is fast.
COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin "${BIN}" \
 && cp "/build/target/release/${BIN}" "/build/bin"

# ── Runtime ────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
ARG BIN

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 bellows \
 && useradd  --system --uid 10001 --gid bellows --home /app --no-create-home --shell /usr/sbin/nologin bellows

WORKDIR /app
COPY --from=builder /build/bin /usr/local/bin/bellows-agent

# Server-mode by default; the example reads BELLOWS_MODEL / BELLOWS_QUESTION
# / etc. from env.
ENV BELLOWS_MODE=server \
    RUST_LOG=info,bellows=debug

USER bellows
EXPOSE 3548

# Use exec form so SIGTERM goes straight to the binary. Bellows handles
# graceful shutdown on Ctrl-C / SIGINT; production senders typically use
# SIGTERM, which the Tokio signal hook also catches.
CMD ["bellows-agent"]
