#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

PORT="${SLACK_DEMO_PORT:-8083}"
LOG_FILE="${SLACK_DEMO_LOG:-/tmp/slack-runner.log}"
PID_FILE="${SLACK_DEMO_PID:-/tmp/slack-runner.pid}"
STREAM_FILE="${SLACK_DEMO_STREAM:-/tmp/slack-demo-sse.log}"
PROVENANCE_DB="${SLACK_DEMO_PROVENANCE_DB:-provenance.db}"

RUNNER_URL="${SLACK_DEMO_RUNNER_URL:-http://127.0.0.1:${PORT}}"
REPOSITORY_URL="${SLACK_DEMO_REPOSITORY_URL:-${RUNNER_URL}/repository}"
STATE_DIR="${SLACK_DEMO_STATE_DIR:-/tmp/slack-demo-runner-state-${PORT}}"
REPOSITORY_DIR="${SLACK_DEMO_REPOSITORY_DIR:-/tmp/slack-demo-repository-${PORT}}"

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
BUILDER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium"

cargo build -p baml-rt-builder --features http-tools --bin baml-agent-builder
cargo build -p baml-agent-runner --features http-tools
RUNNER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium"
if [ ! -x "$RUNNER_BIN" ]; then
  echo "Runner binary not found: $RUNNER_BIN" >&2
  exit 1
fi
if [ ! -x "$BUILDER_BIN" ]; then
  echo "Builder binary not found: $BUILDER_BIN" >&2
  exit 1
fi

mkdir -p "$STATE_DIR" "$REPOSITORY_DIR"

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
  --agent-dir agents/slack-agent \
  --repository-url "$REPOSITORY_URL" \
  --deploy-url "$RUNNER_URL"

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
  curl -s -N -X POST "${RUNNER_URL}/agents/slack-agent/default/a2a/sse" \
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
  echo "Mermaid sequence: curl -sS ${RUNNER_URL}/contexts/${CONTEXT_ID}/mermaid" >&2
fi
if [ -n "$TASK_ID" ]; then
  echo "Captured task id: $TASK_ID" >&2
fi
