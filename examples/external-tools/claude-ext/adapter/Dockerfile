# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for dev/claude-ext (rust source as tool logic).

FROM rust:1-bookworm AS builder
WORKDIR /build
# Build deps for claude-agent-sdk-rs (openssl-sys, pkg-config) — slim base ships neither.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml /build/Cargo.toml
COPY src /build/src
RUN cargo build --release --manifest-path /build/Cargo.toml --features sdk-engine

FROM node:20-bookworm-slim

# Keep SDK subprocess prerequisites minimal and deterministic.
ARG CLAUDE_CODE_NPM_VERSION=2.1.121
# node:20-bookworm-slim ships a `node` user pinned to uid/gid 1000.
# Use 1001 to avoid the collision; uid value itself is not load-bearing.
ARG SANDBOX_UID=1001
ARG SANDBOX_GID=1001

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl python3 util-linux \
    && rm -rf /var/lib/apt/lists/*

# Some microsandbox bind-rootfs exports do not provide the usual /dev stdio
# symlinks. Node/Claude tooling may probe these paths; keep them available
# and pointing at the current process fds.
RUN ln -sf /proc/self/fd/0 /dev/stdin \
    && ln -sf /proc/self/fd/1 /dev/stdout \
    && ln -sf /proc/self/fd/2 /dev/stderr

# Pin Claude Code CLI used by the Rust SDK subprocess (install as root before
# dropping privileges so the global npm prefix stays world-readable).
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_NPM_VERSION}" \
    && claude --version

# Create non-root sandbox user. claude-code refuses
# `--dangerously-skip-permissions` when running as root/sudo, so the adapter
# (and the persistent claude child it spawns) must drop privileges.
# Microsandbox uses the OCI image as a rootfs only — it ignores the `USER`
# directive and the guest agent execs the entrypoint as root. Use a setpriv
# wrapper at `/run-adapter.sh` that drops to ${SANDBOX_UID}:${SANDBOX_GID}
# before exec'ing the real adapter binary.
RUN groupadd --system --gid "${SANDBOX_GID}" sandbox \
    && useradd --system --uid "${SANDBOX_UID}" --gid "${SANDBOX_GID}" \
        --home-dir /home/sandbox --create-home --shell /bin/bash sandbox

ENV NODE_ENV=production \
    HOME=/home/sandbox \
    USER=sandbox \
    XDG_CONFIG_HOME=/home/sandbox/.config \
    XDG_CACHE_HOME=/home/sandbox/.cache

# Pre-create the per-tool home dirs the CLI bootstraps into and the workspace
# dir the SDK uses as cwd. Microsandbox may still launch the adapter with
# HOME=/ and cwd=/, so the adapter also resets HOME and chdirs at startup
# (see src/main.rs::ensure_runtime_home); these directories make sure the
# target paths are ready when that fixup runs.
#
# Seed claude-code onboarding state. claude-code 2.x runs first-run setup
# when ~/.claude is empty: prints welcome banner, expects TTY interaction,
# exits cleanly when stdin is piped — which kills the persistent stream-json
# session the adapter relies on. Pre-populating settings.json + ~/.claude.json
# with onboarding-completed flags skips first-run and lets claude enter the
# stream loop immediately.
RUN mkdir -p /home/sandbox/.claude /home/sandbox/.config /home/sandbox/.cache /home/sandbox/workspace \
    && printf '%s\n' '{"hasCompletedOnboarding":true,"bypassPermissionsModeAccepted":true}' \
        > /home/sandbox/.claude/settings.json \
    && printf '%s\n' '{"hasCompletedOnboarding":true,"hasTrustDialogAccepted":true,"bypassPermissionsModeAccepted":true,"firstStartTime":"2024-01-01T00:00:00.000Z","numStartups":1}' \
        > /home/sandbox/.claude.json \
    && chown -R sandbox:sandbox /home/sandbox /home/sandbox/.claude.json

COPY --from=builder /build/target/release/external-tool /tool-adapter-bin
RUN chmod +x /tool-adapter-bin

# setpriv wrapper installed at `/tool-adapter`: microsandbox provider execs
# `/tool-adapter` literally (microsandbox_provider.rs:301), bypassing the
# Dockerfile ENTRYPOINT. The wrapper drops privs to ${SANDBOX_UID}:${SANDBOX_GID}
# then execs the real binary at `/tool-adapter-bin`. claude-code accepts
# `--dangerously-skip-permissions` once the process is non-root.
RUN printf '#!/bin/sh\nset -eu\nmkdir -p /home/sandbox/.claude/session-env /home/sandbox/.config /home/sandbox/.cache /home/sandbox/workspace\nchown -R %s:%s /home/sandbox 2>/dev/null || true\nchmod -R u+rwX /home/sandbox 2>/dev/null || true\nexec setpriv --reuid=%s --regid=%s --clear-groups --no-new-privs /tool-adapter-bin "$@"\n' \
        "${SANDBOX_UID}" "${SANDBOX_GID}" "${SANDBOX_UID}" "${SANDBOX_GID}" > /tool-adapter \
    && chmod +x /tool-adapter

WORKDIR /home/sandbox/workspace

ENTRYPOINT ["/tool-adapter"]
