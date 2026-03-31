#!/usr/bin/env bash
# Start the runner, wait for readiness, publish+deploy a list of agent directories,
# then stay alive until Ctrl-C (which kills the runner via the trap).
#
# Usage:
#   run-stack.sh <runner_bin> <builder_bin> <http_bind> <runner_url> \
#                <provenance_db> <state_dir> <repository_dir> \
#                [--web-dir <path>] [--event-poll-interval-secs <n>] \
#                <agent_dir> [<agent_dir>...]
set -euo pipefail

RUNNER_BIN="$1";  shift
BUILDER_BIN="$1"; shift
HTTP_BIND="$1";   shift
RUNNER_URL="$1";  shift
PROVENANCE_DB="$1"; shift
STATE_DIR="$1";     shift
REPOSITORY_DIR="$1"; shift

RUNNER_EXTRA_ARGS=()
AGENT_DIRS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --web-dir|--event-poll-interval-secs)
            RUNNER_EXTRA_ARGS+=("$1" "$2"); shift 2 ;;
        *) AGENT_DIRS+=("$1"); shift ;;
    esac
done

if [[ ${#AGENT_DIRS[@]} -eq 0 ]]; then
    echo "run-stack.sh: no agent directories specified" >&2
    exit 1
fi

# Start runner in background.
"$RUNNER_BIN" \
    --serve-http "$HTTP_BIND" \
    --provenance-db "$PROVENANCE_DB" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPOSITORY_DIR" \
    "${RUNNER_EXTRA_ARGS[@]}" &

RUNNER_PID=$!
trap "kill $RUNNER_PID 2>/dev/null; exit 0" INT TERM

# Wait for the runner to accept requests (up to 30 s).
echo "⏳ Waiting for runner at $RUNNER_URL ..."
for i in $(seq 1 30); do
    curl -sf "$RUNNER_URL/agents" > /dev/null 2>&1 && break
    sleep 1
done

# Publish and deploy each agent.
for dir in "${AGENT_DIRS[@]}"; do
    "$BUILDER_BIN" publish \
        --agent-dir "$dir" \
        --repository-url "$RUNNER_URL/repository" \
        --deploy-url "$RUNNER_URL"
done

wait "$RUNNER_PID"
