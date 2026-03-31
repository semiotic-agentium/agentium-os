#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${COORDINATOR_DEMO_PORT:-8082}"
LOG_FILE="${COORDINATOR_DEMO_LOG:-/tmp/coordinator-runner.log}"
PID_FILE="${COORDINATOR_DEMO_PID:-/tmp/coordinator-runner.pid}"
INCLUDE_CLICKUP="${COORDINATOR_DEMO_INCLUDE_CLICKUP:-0}"
INCLUDE_NOTION="${COORDINATOR_DEMO_INCLUDE_NOTION:-1}"
STREAM_FILE="${COORDINATOR_DEMO_STREAM:-/tmp/coordinator-demo-sse.log}"
PROVENANCE_DB="${COORDINATOR_DEMO_PROVENANCE_DB:-provenance.db}"
ENTRY_AGENT="${COORDINATOR_DEMO_ENTRY_AGENT:-coordinator-agent}"

RUNNER_URL="${COORDINATOR_DEMO_RUNNER_URL:-http://127.0.0.1:${PORT}}"
REPOSITORY_URL="${COORDINATOR_DEMO_REPOSITORY_URL:-${RUNNER_URL}/repository}"
STATE_DIR="${COORDINATOR_DEMO_STATE_DIR:-/tmp/coordinator-demo-runner-state-${PORT}}"
REPOSITORY_DIR="${COORDINATOR_DEMO_REPOSITORY_DIR:-/tmp/coordinator-demo-repository-${PORT}}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required but not found in PATH" >&2
  exit 1
fi

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUILDER_BIN="${COORDINATOR_DEMO_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"

cargo build -p baml-rt-builder --features http-tools --bin baml-agent-builder
cargo build -p baml-agent-runner --features http-tools
RUNNER_BIN="${COORDINATOR_DEMO_RUNNER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-runner}"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
fi
if [ ! -x "$BUILDER_BIN" ]; then
  echo "Builder binary not found: $BUILDER_BIN" >&2
  exit 1
fi

mkdir -p "$STATE_DIR" "$REPOSITORY_DIR"

LOADED_AGENTS=("coordinator-agent" "workflow-intake-agent")
PUBLISH_DIRS=(agents/coordinator-agent agents/workflow-intake-agent)
if [ "$INCLUDE_NOTION" = "1" ]; then
  LOADED_AGENTS+=("notion-agent")
  PUBLISH_DIRS+=(agents/notion-agent)
fi
if [ "$INCLUDE_CLICKUP" = "1" ]; then
  LOADED_AGENTS+=("clickup-agent")
  PUBLISH_DIRS+=(agents/clickup-agent)
fi

if [ -f "$PID_FILE" ]; then
  OLD_PID="$(cat "$PID_FILE" || true)"
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$OLD_PID" 2>/dev/null; then
        break
      fi
      sleep 0.25
    done
    if kill -0 "$OLD_PID" 2>/dev/null; then
      kill -9 "$OLD_PID" || true
    fi
  fi
  rm -f "$PID_FILE"
fi

LISTEN_PID="$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | head -n 1 || true)"
if [ -n "$LISTEN_PID" ]; then
  echo "Port ${PORT} already in use by pid ${LISTEN_PID}; stopping it..." >&2
  kill "$LISTEN_PID" || true
  for _ in $(seq 1 20); do
    if ! kill -0 "$LISTEN_PID" 2>/dev/null; then
      break
    fi
    sleep 0.25
  done
  if kill -0 "$LISTEN_PID" 2>/dev/null; then
    kill -9 "$LISTEN_PID" || true
  fi
  if kill -0 "$LISTEN_PID" 2>/dev/null; then
    echo "Failed to free port ${PORT} from pid ${LISTEN_PID}" >&2
    exit 1
  fi
fi

RUST_LOG=${RUST_LOG:-baml_rt_a2a=debug,baml_rt_quickjs=debug,baml_rt_tools=debug} \
  nohup "$RUNNER_BIN" \
    --serve-http "127.0.0.1:${PORT}" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPOSITORY_DIR" \
    --provenance-db "$PROVENANCE_DB" \
    >"$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"

port_listening() {
  curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1
}

for _ in $(seq 1 120); do
  if port_listening; then
    break
  fi
  sleep 0.5
done

if ! port_listening; then
  echo "Runner failed to become ready on ${RUNNER_URL}. See $LOG_FILE" >&2
  exit 1
fi

for dir in "${PUBLISH_DIRS[@]}"; do
  "$BUILDER_BIN" publish \
    --agent-dir "$dir" \
    --repository-url "$REPOSITORY_URL" \
    --deploy-url "$RUNNER_URL"
done

echo "Runner ready at ${RUNNER_URL}" >&2
echo "Published agents: ${LOADED_AGENTS[*]}" >&2

if [ "${COORDINATOR_DEMO_NO_STREAM:-0}" = "1" ]; then
  echo "UI mode: point your chat UI backend to ${RUNNER_URL} and select ${ENTRY_AGENT}." >&2
  exit 0
fi

if [ -n "${COORDINATOR_DEMO_TEXT:-}" ]; then
  TEXT="$COORDINATOR_DEMO_TEXT"
else
  TEXT="Can you tell me what the research team are up to and what actionable goals they have?"
fi

echo "Streaming demo request to ${ENTRY_AGENT} on :${PORT} (provenance db: ${PROVENANCE_DB})" >&2

NOW_MS="$(( $(date +%s) * 1000 ))"
MSG_ID="msg-${NOW_MS}"
CORR_ID="corr-${NOW_MS}-$$"
jq -n --arg text "$TEXT" --arg msg_id "$MSG_ID" --arg corr_id "$CORR_ID" \
  '{jsonrpc:"2.0", method:"message.sendStream", params:{message:{messageId:$msg_id,role:"ROLE_USER",parts:[{text:$text}]}}, id:$corr_id}' | \
  curl -s -N -X POST "${RUNNER_URL}/agents/${ENTRY_AGENT}/default/a2a/sse" \
    -H "Content-Type: application/json" \
    -H "Accept: text/event-stream" \
    -d @- | tee "$STREAM_FILE"

CONTEXT_ID="$(
  (
    sed -n 's/^data: //p' "$STREAM_FILE" \
      | jq -r '.. | objects | .contextId? // empty' || true
  ) 2>/dev/null | tail -n 1
)"
TASK_ID="$(
  (
    sed -n 's/^data: //p' "$STREAM_FILE" \
      | jq -r '.result.chunk.task.id? // empty' || true
  ) 2>/dev/null | tail -n 1
)"

if [ -n "$CONTEXT_ID" ]; then
  echo "Captured context id: $CONTEXT_ID" >&2
  echo "Export sequence diagram: just provenance-mermaid $CONTEXT_ID" >&2
fi
if [ -n "$TASK_ID" ]; then
  echo "Captured task id: $TASK_ID" >&2
fi
