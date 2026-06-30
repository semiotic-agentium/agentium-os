#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
TOOL_DIR="$ROOT/examples/external-tools/claude-ext"
TOOL_TMP_DIR="$TOOL_DIR/.tmp"
ROOTFS="$TOOL_TMP_DIR/dev-claude-ext-rootfs"
AGENT_DIR="examples/agents/claude-agent"
AGENT_NAME="claude-agent"
SANDBOX_IMAGE="dev-claude-ext-sandbox:local"
RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:18080}"
REPOSITORY_URL="${REPOSITORY_URL:-$RUNNER_URL/repository}"

# This demo uses registry-approved snapshots bundled into the agent package.
# Do not export BAML_EXTERNAL_TOOLS_DIR here: that env is dev fallback only and
# would duplicate the packaged resolver after publish/deploy.
#
# Allow bind/rootfs images materialized by `setup_bind_sandbox.sh`.
# Keep allowlist narrow: rootfs only, not whole external-tool directory.
export BAML_SANDBOX_BIND_ROOTS="$ROOTFS"

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
  enable    Approve/import the Claude external-tool snapshot into the repository registry
  runner    Prepare the sandbox, then start baml-agent-runner on 127.0.0.1:18080
  push      Enable registry snapshot, then publish/deploy examples/agents/claude-agent
  chat      Run push, then connect to claude-agent with agentium chat

Typical flow:
  # Terminal 1
  scripts/claude_sandbox.sh runner

  # Terminal 2
  scripts/claude_sandbox.sh chat

Environment exported by this script:
  BAML_SANDBOX_BIND_ROOTS=$BAML_SANDBOX_BIND_ROOTS
  CLAUDE_EXT_USE_SHELL_PIPELINE=$CLAUDE_EXT_USE_SHELL_PIPELINE

Environment:
  RUNNER_URL=$RUNNER_URL
  REPOSITORY_URL=$REPOSITORY_URL

Sandbox paths:
  external BAML tool dir: $TOOL_DIR
  bind rootfs:            $ROOTFS
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
    AGENT_PLATFORM_CMD="cargo run -q -p agentium --" \
        "$TOOL_DIR/setup_bind_sandbox.sh" \
        --image "$SANDBOX_IMAGE" \
        --rootfs "$ROOTFS" \
        --force

    log_step "Sandbox bind rootfs ready"
}

enable_external_tool() {
    if [[ ! -d "$ROOTFS" ]]; then
        echo "missing sandbox rootfs: $ROOTFS" >&2
        echo "run scripts/claude_sandbox.sh prepare first (or start scripts/claude_sandbox.sh runner)" >&2
        exit 1
    fi

    log_step "Approving/importing Claude external-tool snapshot into $REPOSITORY_URL"
    cargo run -q -p agentium -- external-tool enable \
        "$TOOL_DIR" \
        --sandbox-rootfs "$ROOTFS" \
        --repository-url "$REPOSITORY_URL" \
        --yes
}

push_agent() {
    enable_external_tool
    log_step "Publishing and deploying $AGENT_DIR"
    cargo run -q -p agentium -- push \
        --agents "$AGENT_DIR" \
        --repository-url "$REPOSITORY_URL" \
        --url "$RUNNER_URL"
}

cd "$ROOT"

cmd="${1:-runner}"
case "$cmd" in
    prepare)
        prepare
        ;;
    enable)
        enable_external_tool
        ;;
    runner)
        prepare
        log_step "Starting baml-agent-runner on 127.0.0.1:18080"
        echo "    In another terminal, run: scripts/claude_sandbox.sh chat"
        exec cargo run -p agentium -- serve --all-features -- \
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
        exec cargo run -p agentium -- chat --agent "$AGENT_NAME"
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
