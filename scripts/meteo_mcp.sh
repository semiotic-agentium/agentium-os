#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
SERVER_ID="meteo"
SERVER_DIR="examples/external-mcp-servers/meteo-mcp"
SERVER_CONFIG_TMPL="$SERVER_DIR/mcp-servers.json.tmpl"
SERVER_CONFIG="$HOME/.agentium-os/mcp-servers.json"
AGENT_DIR="examples/agents/meteo-mcp-agent"
AGENT_NAME="meteo-mcp-agent"
RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:18080}"
REPOSITORY_URL="${REPOSITORY_URL:-$RUNNER_URL/repository}"
HTTP_BIND="${HTTP_BIND:-127.0.0.1:18080}"
PACKAGE_OUT="${PACKAGE_OUT:-/tmp/meteo-mcp-agent.tar.gz}"
# Keep this local MCP demo responsive even when fastembed/JINA model caches are
# missing or corrupted. Drift scoring will lazily disable/fall back on first use.
export BAML_SKIP_DRIFT_MODEL_WARMUP="${BAML_SKIP_DRIFT_MODEL_WARMUP:-1}"
# Keep the local demo quiet when no OTEL collector is running.
export OTEL_SDK_DISABLED="${OTEL_SDK_DISABLED:-true}"

usage() {
    cat <<EOF
Usage: scripts/meteo_mcp.sh <command>

Commands:
  config    Render MCP server runtime config only
  enable    Discover/approve meteo MCP schema directly into the repository registry
  prepare   Render config, enable registry schema, and package the agent locally
  runner    Render config, then start baml-agent-runner on $HTTP_BIND
  push      Enable registry schema, then publish+deploy $AGENT_DIR to $RUNNER_URL
  chat      Run push, then connect to $AGENT_NAME with cargo-agent-platform chat
  review    Show registry state for the meteo MCP server

Typical flow:
  # Terminal 1
  scripts/meteo_mcp.sh runner

  # Terminal 2
  scripts/meteo_mcp.sh chat

Environment:
  RUNNER_URL=$RUNNER_URL
  REPOSITORY_URL=$REPOSITORY_URL
  HTTP_BIND=$HTTP_BIND
  PACKAGE_OUT=$PACKAGE_OUT
  BAML_SKIP_DRIFT_MODEL_WARMUP=$BAML_SKIP_DRIFT_MODEL_WARMUP

Notes:
  - The registry is now the schema source of truth for this demo.
  - ~/.agentium-os/mcp-servers.json remains local runtime config: command,
    args, non-secret env, and secret env var names.
  - Schema snapshots are written to the repository registry, not
    ~/.agentium-os/mcp.
  - For the normal two-terminal demo, start runner first, then run chat/push so
    the repository endpoint is available for MCP enable + publish.
EOF
}

log_step() {
    printf '\n==> %s\n' "$*"
}

builder() {
    cargo run -q -p baml-rt-builder --all-features --bin baml-agent-builder -- "$@"
}

agent_platform() {
    cargo run -q -p cargo-agent-platform -- "$@"
}

render_config() {
    log_step "Rendering MCP server runtime config: $SERVER_CONFIG"
    mkdir -p "$(dirname "$SERVER_CONFIG")"
    sed "s|\${ROOT_DIR}|${ROOT}|g" "$SERVER_CONFIG_TMPL" > "$SERVER_CONFIG"
}

enable_registry() {
    cd "$ROOT"
    render_config
    log_step "Discovering and approving MCP server '$SERVER_ID' into registry $REPOSITORY_URL"
    agent_platform mcp enable "$SERVER_ID" \
        --config "$SERVER_CONFIG" \
        --repository-url "$REPOSITORY_URL" \
        --yes

    log_step "Registry versions for '$SERVER_ID'"
    agent_platform mcp versions "$SERVER_ID" --repository-url "$REPOSITORY_URL"
}

package_agent() {
    log_step "Packaging $AGENT_DIR using MCP registry $REPOSITORY_URL"
    BAML_REGISTRY_URL="$REPOSITORY_URL" \
        builder package --agent-dir "$AGENT_DIR" --output "$PACKAGE_OUT"

    log_step "Generated MCP BAML types"
    rg "McpMeteoGetMeteo" "$AGENT_DIR/src/baml-runtime.d.ts" | head || true
}

prepare() {
    cd "$ROOT"
    enable_registry
    package_agent
}

push_agent() {
    cd "$ROOT"
    enable_registry
    log_step "Publishing and deploying $AGENT_DIR to $RUNNER_URL"
    builder publish \
        --agent-dir "$AGENT_DIR" \
        --repository-url "$REPOSITORY_URL" \
        --deploy-url "$RUNNER_URL" \
        --message "meteo-mcp demo"
}

review() {
    cd "$ROOT"
    log_step "Registry state for MCP server '$SERVER_ID'"
    agent_platform mcp server "$SERVER_ID" --repository-url "$REPOSITORY_URL"
    agent_platform mcp tool "mcp/$SERVER_ID/get_meteo" --repository-url "$REPOSITORY_URL"
}

cd "$ROOT"
cmd="${1:-runner}"
case "$cmd" in
    config)
        render_config
        ;;
    enable)
        enable_registry
        ;;
    prepare)
        prepare
        ;;
    runner)
        render_config
        log_step "Starting baml-agent-runner on $HTTP_BIND"
        echo "    In another terminal, run: scripts/meteo_mcp.sh chat"
        exec cargo run -p baml-agent-runner --all-features -- \
            --a2a-stdio \
            --serve-http "$HTTP_BIND" \
            --provenance-db provenance.db
        ;;
    push)
        push_agent
        ;;
    chat)
        push_agent
        log_step "Connecting to $AGENT_NAME"
        exec cargo run -q -p cargo-agent-platform -- chat --agent "$AGENT_NAME" --url "$RUNNER_URL"
        ;;
    review)
        review
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
