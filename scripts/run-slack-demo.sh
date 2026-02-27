#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${SLACK_DEMO_PORT:-8083}"
LOG_FILE="${SLACK_DEMO_LOG:-/tmp/slack-runner.log}"
PID_FILE="${SLACK_DEMO_PID:-/tmp/slack-runner.pid}"
PACKAGE_FILE="${SLACK_DEMO_PACKAGE:-/tmp/slack-agent.tar.gz}"
STREAM_FILE="${SLACK_DEMO_STREAM:-/tmp/slack-demo-sse.log}"
PROVENANCE_DB="${SLACK_DEMO_PROVENANCE_DB:-provenance.db}"

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
  package --agent-dir agents/slack-agent --output "$PACKAGE_FILE"

cargo build -p baml-agent-runner --features http-tools
RUNNER_BIN="${SLACK_DEMO_RUNNER_BIN:-target/debug/baml-agent-runner}"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
fi

if [ -f "$PID_FILE" ]; then
  OLD_PID="$(cat "$PID_FILE" || true)"
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" || true
    sleep 0.5
  fi
  rm -f "$PID_FILE"
fi

RUST_LOG=${RUST_LOG:-baml_rt_a2a=debug,baml_rt_quickjs=debug,baml_rt_tools=debug} \
  nohup "$RUNNER_BIN" \
    "$PACKAGE_FILE" \
    --serve-http "127.0.0.1:${PORT}" \
    --provenance-db "$PROVENANCE_DB" \
    >"$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"

for _ in $(seq 1 60); do
  if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

if ! lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "Runner failed to start on port $PORT. See $LOG_FILE" >&2
  exit 1
fi

if [ -n "${SLACK_DEMO_TEXT:-}" ]; then
  TEXT="$SLACK_DEMO_TEXT"
elif [ -n "${SLACK_DEMO_THREAD_URL:-}" ]; then
  TEXT="Extract todos from this Slack thread ${SLACK_DEMO_THREAD_URL}. Include owner, due date, confidence, and sources."
elif [ -n "${SLACK_DEMO_CHANNEL_ID:-}" ]; then
  TEXT="Extract todos from channel ${SLACK_DEMO_CHANNEL_ID} oldest=${SLACK_DEMO_OLDEST:-} latest=${SLACK_DEMO_LATEST:-}. Include owner, due date, confidence, and sources."
else
  TEXT="Extract todos from this Slack thread https://acme.slack.com/archives/C12345678/p1735689600000000 and include owner, due date, confidence, and sources."
fi

echo "Streaming demo request to slack-agent on :${PORT} (provenance db: ${PROVENANCE_DB})" >&2

jq -n --arg text "$TEXT" \
  '{jsonrpc:"2.0", method:"message.sendStream", params:{message:{messageId:"msg-3",role:"ROLE_USER",parts:[{text:$text}]}}, id:"corr-1700000000000-3"}' | \
  curl -s -N -X POST "http://127.0.0.1:${PORT}/agents/slack-agent/default/a2a/sse" \
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
