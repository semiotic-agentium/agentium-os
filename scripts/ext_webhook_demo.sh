#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0
#
# Event-driven demo (NO chat): a raw external datasource feeds webhook events to
# a subscribing agent's onDispatch. Mirrors scripts/claude_sandbox.sh ergonomics.
#
#   Terminal 1:  scripts/ext_webhook_demo.sh runner    # runner + datasource; live logs here
#   Terminal 2:  scripts/ext_webhook_demo.sh push      # publish + deploy the consumer agent (once)
#   Terminal 2:  scripts/ext_webhook_demo.sh trigger   # POST a deploy-health event; watch Terminal 1
#
# In Terminal 1 you should see, per trigger:
#   A2aAgent::handle_dispatch envelope routing=deploy-health ...
#   event delivery complete producer_key=external-datasource:...:events matched=1 accepted=1

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

PORT="${DEPLOY_HEALTH_PORT:-18080}"
RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:${PORT}}"
REPOSITORY_URL="${REPOSITORY_URL:-$RUNNER_URL/repository}"
STATE_DIR="${DEPLOY_HEALTH_STATE_DIR:-/tmp/deploy-health-runner-state-${PORT}}"
REPOSITORY_DIR="${DEPLOY_HEALTH_REPOSITORY_DIR:-/tmp/deploy-health-repository-${PORT}}"

DATASOURCE_CONFIG="examples/external-tools/deploy-health-datasource/runner.toml"
AGENT_DIR="examples/agents/deploy-health-consumer"
WEBHOOK_PATH="/webhooks/ext/examples/deploy-health-datasource/events"

log_step() { printf '\n==> %s\n' "$*"; }

usage() {
    cat <<EOF
Usage: scripts/ext_webhook_demo.sh <command> [args]

Commands:
  runner                       Start baml-agent-runner on ${RUNNER_URL} with the
                               deploy-health datasource active (foreground; logs stream here).
  push                         Publish + deploy ${AGENT_DIR} into the running runner.
  trigger [service] [status]   POST one deploy-health webhook event (default: checkout degraded).
  stop                         Stop the runner on :${PORT} and remove demo state.
  help                         Show this help.

Typical flow:
  # Terminal 1
  scripts/ext_webhook_demo.sh runner
  # Terminal 2 (once the runner is ready)
  scripts/ext_webhook_demo.sh push
  scripts/ext_webhook_demo.sh trigger
  scripts/ext_webhook_demo.sh trigger payments down

Environment:
  DEPLOY_HEALTH_PORT=${PORT}
  RUNNER_URL=${RUNNER_URL}
  REPOSITORY_URL=${REPOSITORY_URL}
  STATE_DIR=${STATE_DIR}
  REPOSITORY_DIR=${REPOSITORY_DIR}
EOF
}

free_port() {
    local listen_pid
    listen_pid="$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | head -n 1 || true)"
    if [ -n "$listen_pid" ]; then
        echo "Port ${PORT} in use by pid ${listen_pid}; stopping it..." >&2
        kill "$listen_pid" 2>/dev/null || true
        for _ in $(seq 1 20); do
            kill -0 "$listen_pid" 2>/dev/null || break
            sleep 0.25
        done
        kill -9 "$listen_pid" 2>/dev/null || true
    fi
}

cmd="${1:-help}"
case "$cmd" in
    runner)
        free_port
        mkdir -p "$STATE_DIR" "$REPOSITORY_DIR"
        log_step "Starting baml-agent-runner on ${RUNNER_URL} (deploy-health datasource active; event-driven, no chat)"
        echo "    Terminal 2: scripts/ext_webhook_demo.sh push      # deploy the consumer agent (once)"
        echo "    Terminal 2: scripts/ext_webhook_demo.sh trigger   # POST a deploy-health event"
        exec env RUST_LOG="${RUST_LOG:-info,baml_rt_a2a=debug}" \
            cargo run -p agentium -- serve -- \
            --serve-http "127.0.0.1:${PORT}" \
            --runner-config "$DATASOURCE_CONFIG" \
            --state-dir "$STATE_DIR" \
            --repository-dir "$REPOSITORY_DIR" \
            --repository-url "$REPOSITORY_URL" \
            --event-poll-interval-secs 1
        ;;
    push)
        log_step "Publishing + deploying ${AGENT_DIR} into ${RUNNER_URL}"
        exec cargo run -q -p agentium -- push \
            --agents "$AGENT_DIR" \
            --repository-url "$REPOSITORY_URL" \
            --url "$RUNNER_URL" \
            --origin original
        ;;
    trigger)
        service="${2:-checkout}"
        status="${3:-degraded}"
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        deploy_id="d-$(date +%s)"
        body="$(printf '{"service":"%s","environment":"prod","status":"%s","deploy_id":"%s","observed_at":"%s"}' \
            "$service" "$status" "$deploy_id" "$ts")"
        log_step "POST ${WEBHOOK_PATH} (service=${service} status=${status})"
        echo "    body: ${body}"
        curl -sS -i -X POST "${RUNNER_URL}${WEBHOOK_PATH}" \
            -H 'content-type: application/json' \
            -d "$body"
        echo
        ;;
    stop)
        free_port
        rm -rf "$STATE_DIR" "$REPOSITORY_DIR"
        log_step "Stopped runner on :${PORT} and removed demo state"
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
