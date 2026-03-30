#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${NOTION_DEMO_PORT:-8081}"
LOG_FILE="${NOTION_DEMO_LOG:-/tmp/notion-runner.log}"
PID_FILE="${NOTION_DEMO_PID:-/tmp/notion-runner.pid}"
STREAM_FILE="${NOTION_DEMO_STREAM:-/tmp/notion-demo-sse.log}"
PROVENANCE_DB="${NOTION_DEMO_PROVENANCE_DB:-provenance.db}"

RUNNER_URL="${NOTION_DEMO_RUNNER_URL:-http://127.0.0.1:${PORT}}"
REPOSITORY_URL="${NOTION_DEMO_REPOSITORY_URL:-${RUNNER_URL}/repository}"
STATE_DIR="${NOTION_DEMO_STATE_DIR:-/tmp/notion-demo-runner-state-${PORT}}"
REPOSITORY_DIR="${NOTION_DEMO_REPOSITORY_DIR:-/tmp/notion-demo-repository-${PORT}}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required but not found in PATH" >&2
  exit 1
fi

# Load .env if present
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUILDER_BIN="${NOTION_DEMO_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"

cargo build -p baml-rt-builder --features http-tools --bin baml-agent-builder
cargo build -p baml-agent-runner --features http-tools
RUNNER_BIN="${NOTION_DEMO_RUNNER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-runner}"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
fi
if [ ! -x "$BUILDER_BIN" ]; then
  echo "Builder binary not found: $BUILDER_BIN" >&2
  exit 1
fi

mkdir -p "$STATE_DIR" "$REPOSITORY_DIR"

# Stop existing runner if still running
if [ -f "$PID_FILE" ]; then
  OLD_PID="$(cat "$PID_FILE" || true)"
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" || true
    sleep 0.5
  fi
  rm -f "$PID_FILE"
fi

# Start runner (no positional packages — deploy via repository publish + POST /deploy)
RUST_LOG=${RUST_LOG:-baml_rt_a2a=debug,baml_rt_quickjs=debug,baml_rt_tools=debug} \
  nohup "$RUNNER_BIN" \
    --serve-http "127.0.0.1:${PORT}" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPOSITORY_DIR" \
    --provenance-db "$PROVENANCE_DB" \
    >"$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"

for _ in $(seq 1 120); do
  if curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

if ! curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
  echo "Runner failed to become ready on ${RUNNER_URL}. See $LOG_FILE" >&2
  exit 1
fi

"$BUILDER_BIN" publish \
  --agent-dir agents/notion-agent \
  --repository-url "$REPOSITORY_URL" \
  --deploy-url "$RUNNER_URL"

if [ -n "${NOTION_DEMO_TEXT:-}" ]; then
  TEXT="$NOTION_DEMO_TEXT"
elif [ -n "${NOTION_DEMO_PAGE_ID:-}" ]; then
  TEXT="Summarize this Notion page ${NOTION_DEMO_PAGE_ID}. Focus on commitments, conflicts, missing info, and source links."
else
  TEXT="What are we working on right now? Search Notion and summarize commitments, conflicts, and missing info with sources."
fi

echo "Streaming demo request to notion-agent on :${PORT} (provenance db: ${PROVENANCE_DB})" >&2

jq -n --arg text "$TEXT" \
  '{jsonrpc:"2.0", method:"message.sendStream", params:{message:{messageId:"msg-2",role:"ROLE_USER",parts:[{text:$text}]}}, id:"corr-1700000000000-2"}' | \
  curl -s -N -X POST "${RUNNER_URL}/agents/notion-agent/default/a2a/sse" \
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
