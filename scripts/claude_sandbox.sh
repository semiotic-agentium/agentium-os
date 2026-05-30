#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
TOOL_DIR="$ROOT/examples/external-tools/claude-ext"
TOOL_TMP_DIR="$TOOL_DIR/.tmp"
AGENT_DIR="examples/agents/claude-agent"
AGENT_NAME="claude-agent"
SANDBOX_IMAGE="dev-claude-ext-sandbox:local"

export BAML_EXTERNAL_TOOLS_DIR="$TOOL_DIR"
# Allow bind/rootfs images materialized by `setup_bind_sandbox.sh`.
# The generated sandbox rootfs lives under `$TOOL_DIR/.tmp`, so keep the
# allowlist narrow instead of allowing the whole external-tool directory.
export BAML_SANDBOX_BIND_ROOTS="$TOOL_TMP_DIR"

# Claude-in-sandbox currently supports two launch strategies:
#   1. a simple direct bash/script pipeline
#   2. Rust's tokio::process::Command path
# Both work today. We plan to keep the simpler Command-based path long-term and
# remove this comparison flag once the shell-pipeline fallback is no longer useful.
export CLAUDE_EXT_USE_SHELL_PIPELINE=0

usage() {
    cat <<EOF
Usage: scripts/claude_sandbox.sh <command>

Commands:
  prepare   Rebuild the Claude sandbox bind/rootfs under examples/external-tools/claude-ext/.tmp
  runner    Prepare the sandbox, then start baml-agent-runner on 127.0.0.1:18080
  push      Publish and deploy examples/agents/claude-agent to the running runner
  chat      Run push, then connect to claude-agent with cargo-agent-platform chat

Typical flow:
  # Terminal 1
  scripts/claude_sandbox.sh runner

  # Terminal 2
  scripts/claude_sandbox.sh chat

Environment exported by this script:
  BAML_EXTERNAL_TOOLS_DIR=$BAML_EXTERNAL_TOOLS_DIR
  BAML_SANDBOX_BIND_ROOTS=$BAML_SANDBOX_BIND_ROOTS
  CLAUDE_EXT_USE_SHELL_PIPELINE=$CLAUDE_EXT_USE_SHELL_PIPELINE
EOF
}

log_step() {
    printf '\n==> %s\n' "$*"
}

prepare() {
    log_step "Preparing Claude sandbox bind rootfs"
    log_step "Removing previous dev image if present: $SANDBOX_IMAGE"
    if docker image inspect "$SANDBOX_IMAGE" >/dev/null 2>&1; then
        # Preserve the previous helper's behavior: attempt to remove the old
        # image before rebuilding, but do not make this cleanup fatal. `-f`
        # handles stopped containers that still reference the image; Docker may
        # still refuse if a running container is actively using it.
        if docker image rm -f "$SANDBOX_IMAGE"; then
            echo "    removed image"
        else
            echo "    warning: could not remove image; continuing with setup" >&2
        fi
    else
        echo "    image not present; skipping removal"
    fi

    log_step "Removing previous bind/rootfs directory: $TOOL_TMP_DIR"
    rm -rf "$TOOL_TMP_DIR"

    log_step "Running setup_bind_sandbox.sh --force (builds/materializes sandbox rootfs)"
    "$TOOL_DIR/setup_bind_sandbox.sh" --force

    log_step "Sandbox bind rootfs ready"
}

push_agent() {
    log_step "Publishing and deploying $AGENT_DIR"
    cargo run -q -p cargo-agent-platform -- push --agents "$AGENT_DIR"
}

cd "$ROOT"

cmd="${1:-runner}"
case "$cmd" in
    prepare)
        prepare
        ;;
    runner)
        prepare
        log_step "Starting baml-agent-runner on 127.0.0.1:18080"
        echo "    In another terminal, run: scripts/claude_sandbox.sh chat"
        exec cargo run -p baml-agent-runner --all-features -- \
            --a2a-stdio \
            --serve-http 127.0.0.1:18080 \
            --provenance-db provenance.db
        ;;
    push)
        push_agent
        ;;
    chat)
        push_agent
        log_step "Connecting to $AGENT_NAME"
        exec cargo run -p cargo-agent-platform -- chat --agent "$AGENT_NAME"
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        echo "unknown command: $cmd" >&2
        usage >&2
        exit 2
        ;;
esac
