#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${COORDINATOR_DEMO_PORT:-8082}"
LOG_FILE="${COORDINATOR_DEMO_LOG:-/tmp/coordinator-runner.log}"
PID_FILE="${COORDINATOR_DEMO_PID:-/tmp/coordinator-runner.pid}"
COORDINATOR_PACKAGE_FILE="${COORDINATOR_DEMO_PACKAGE:-/tmp/coordinator-agent.tar.gz}"
NOTION_PACKAGE_FILE="${COORDINATOR_DEMO_NOTION_PACKAGE:-/tmp/notion-agent.tar.gz}"
STREAM_FILE="${COORDINATOR_DEMO_STREAM:-/tmp/coordinator-demo-sse.log}"
PROVENANCE_DB="${COORDINATOR_DEMO_PROVENANCE_DB:-provenance.db}"
ENTRY_AGENT="${COORDINATOR_DEMO_ENTRY_AGENT:-coordinator-agent}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- \
  package --agent-dir agents/notion-agent --output "$NOTION_PACKAGE_FILE"

cargo run -p baml-rt-builder --features http-tools --bin baml-agent-builder -- \
  package --agent-dir agents/coordinator-agent --output "$COORDINATOR_PACKAGE_FILE"

cargo build -p baml-agent-runner --features http-tools
RUNNER_BIN="${COORDINATOR_DEMO_RUNNER_BIN:-target/debug/baml-agent-runner}"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
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

RUST_LOG=${RUST_LOG:-baml_rt_a2a=debug,baml_rt_quickjs=debug,baml_rt_tools=debug} \
  nohup "$RUNNER_BIN" \
    "$COORDINATOR_PACKAGE_FILE" \
    "$NOTION_PACKAGE_FILE" \
    --serve-http "127.0.0.1:${PORT}" \
    --provenance-db "$PROVENANCE_DB" \
    >"$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"

port_listening() {
  nc -z 127.0.0.1 "$PORT" 2>/dev/null || lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1
}

for _ in $(seq 1 60); do
  if port_listening; then
    break
  fi
  sleep 0.5
done

if ! port_listening; then
  echo "Runner failed to start on port $PORT. See $LOG_FILE" >&2
  exit 1
fi

echo "Runner ready at http://127.0.0.1:${PORT}" >&2
echo "Loaded agents: coordinator-agent, notion-agent" >&2

if [ "${COORDINATOR_DEMO_NO_STREAM:-0}" = "1" ]; then
  echo "UI mode: point your chat UI backend to http://127.0.0.1:${PORT} and select ${ENTRY_AGENT}." >&2
  exit 0
fi

if [ -n "${COORDINATOR_DEMO_TEXT:-}" ]; then
  TEXT="$COORDINATOR_DEMO_TEXT"
else
  TEXT="Can you tell me what the research team are up to and what actionable goals they have?"
fi

echo "Streaming demo request to ${ENTRY_AGENT} on :${PORT} (provenance db: ${PROVENANCE_DB})" >&2

MSG_ID="msg-$(date +%s%3N)"
CORR_ID="corr-$(date +%s%3N)-$$"
jq -n --arg text "$TEXT" --arg msg_id "$MSG_ID" --arg corr_id "$CORR_ID" \
  '{jsonrpc:"2.0", method:"message.sendStream", params:{message:{messageId:$msg_id,role:"ROLE_USER",parts:[{text:$text}]}}, id:$corr_id}' | \
  curl -s -N -X POST "http://127.0.0.1:${PORT}/agents/${ENTRY_AGENT}/default/a2a/sse" \
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
