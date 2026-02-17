#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${NOTION_DEMO_PORT:-8081}"
LOG_FILE="${NOTION_DEMO_LOG:-/tmp/notion-runner.log}"
PID_FILE="${NOTION_DEMO_PID:-/tmp/notion-runner.pid}"
FALKORDB_FLAG="${NOTION_DEMO_FALKORDB_FLAG:-/tmp/notion-falkordb.started}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi

# Load .env if present
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

# Optionally manage FalkorDB
if [ "${NOTION_DEMO_FALKORDB:-1}" != "0" ]; then
  if command -v docker >/dev/null 2>&1; then
    ./scripts/falkordb.sh up >/dev/null 2>&1 || true
    touch "$FALKORDB_FLAG"
  else
    echo "docker not found; skipping FalkorDB startup" >&2
  fi
fi

# Build agent package
cargo run -p baml-rt-builder --features notion --bin baml-agent-builder -- \
  package -a agents/notion-agent -o /tmp/notion-agent.tar.gz

# Build runner binary so we can track its PID (cargo run wraps the process)
cargo build -p baml-agent-runner --features notion
RUNNER_BIN="${NOTION_DEMO_RUNNER_BIN:-target/debug/baml-agent-runner}"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
fi

# Stop existing runner if still running
if [ -f "$PID_FILE" ]; then
  OLD_PID="$(cat "$PID_FILE" || true)"
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    kill "$OLD_PID" || true
    sleep 0.5
  fi
  rm -f "$PID_FILE"
fi

# Start runner in background
RUST_LOG=${RUST_LOG:-baml_rt_a2a=debug,baml_rt_quickjs=debug,baml_rt_tools=debug} \
  nohup "$RUNNER_BIN" \
    --serve-http 127.0.0.1:"$PORT" \
    --falkordb-url redis://127.0.0.1:6379 \
    /tmp/notion-agent.tar.gz \
    >"$LOG_FILE" 2>&1 &

echo $! > "$PID_FILE"

# Wait for server
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

DEFAULT_PAGE_ID="303cff78-8181-809b-9e8c-de431eb8c30e"
if [ -n "${NOTION_DEMO_TEXT:-}" ]; then
  TEXT="$NOTION_DEMO_TEXT"
else
  TEXT="Summarize this Notion page ${DEFAULT_PAGE_ID}. Focus on commitments, conflicts, missing info, and be a little funny."
fi

jq -n --arg text "$TEXT" \
  '{jsonrpc:"2.0", method:"message.sendStream", params:{message:{messageId:"msg-2",role:"ROLE_USER",parts:[{text:$text}]}}, id:"corr-1700000000000-2"}' | \
  curl -s -N -X POST "http://127.0.0.1:${PORT}/agents/notion-agent/default/a2a/sse" \
    -H "Content-Type: application/json" \
    -H "Accept: text/event-stream" \
    -d @-
