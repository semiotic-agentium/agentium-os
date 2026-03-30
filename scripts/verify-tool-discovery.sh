#!/usr/bin/env bash
# Ad hoc verification: publish+deploy tool-discovery-demo, send messages and expect
# responses that used system/discover_tools (e.g. mentions "support/calculate" or "Notion").
# Usage: ./scripts/verify-tool-discovery.sh [--build]
# Requires: OPENROUTER_API_KEY in env or .env for LLM (ChooseDiscoverToolsQuery).

set -e
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:8081}"
RUNNER_URL="http://${BIND}"
REPOSITORY_URL="${RUNNER_URL}/repository"
AGENT_DIR="tests/fixtures/agents/tool-discovery-demo"

VERIFY_TMP="${VERIFY_TOOL_TMPDIR:-$(mktemp -d "${TMPDIR:-/tmp}/verify-tool-discovery-XXXXXX")}"
STATE_DIR="${VERIFY_TOOL_STATE_DIR:-$VERIFY_TMP/state}"
REPO_DIR="${VERIFY_TOOL_REPOSITORY_DIR:-$VERIFY_TMP/repository}"
mkdir -p "$STATE_DIR" "$REPO_DIR"

do_build=false
for arg in "$@"; do
  [[ "$arg" == "--build" ]] && do_build=true
done

RUNNER_PID=""
if ! curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "${RUNNER_URL}/agents" 2>/dev/null | grep -q 200; then
  echo "Starting runner on $BIND (state/repo under $VERIFY_TMP)..."
  cargo build -p baml-agent-runner -q
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  RUNNER_BIN="${VERIFY_TOOL_RUNNER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-runner}"
  RUST_LOG="${RUST_LOG:-error}" \
    "$RUNNER_BIN" \
    --serve-http "$BIND" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPO_DIR" \
    &
  RUNNER_PID=$!
  trap 'kill $RUNNER_PID 2>/dev/null || true' EXIT
  echo "Waiting for server..."
  for i in $(seq 1 30); do
    if curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
      break
    fi
    [[ -n "$RUNNER_PID" ]] && ! kill -0 "$RUNNER_PID" 2>/dev/null && exit 1
    sleep 1
  done
  if ! curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
    echo "Runner failed to become ready." >&2
    exit 1
  fi

  cargo build -p baml-rt-builder -q --bin baml-agent-builder
  BUILDER_BIN="${VERIFY_TOOL_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"
  echo "Publishing $AGENT_DIR ..."
  "$BUILDER_BIN" publish --agent-dir "$AGENT_DIR" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
elif [[ "$do_build" == "true" ]]; then
  echo "Re-publishing (--build) against existing server on $BIND..."
  cargo build -p baml-rt-builder -q --bin baml-agent-builder
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  BUILDER_BIN="${VERIFY_TOOL_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"
  "$BUILDER_BIN" publish --agent-dir "$AGENT_DIR" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
fi

# Resolve agent name from manifest (tool-discovery-demo)
AGENT_NAME="tool-discovery-demo"
INSTANCE="default"

echo ""
echo "=== POST message: 'what tools do you have for calculate?' ==="
curl -s -X POST "${RUNNER_URL}/agents/$AGENT_NAME/$INSTANCE/a2a" \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message.sendStream",
    "params": {
      "message": {
        "messageId": "verify-1",
        "role": "user",
        "parts": [{"text": "what tools do you have for calculate?"}]
      }
    },
    "id": "verify-tool-discovery-1"
  }' | head -100

echo ""
echo "=== POST message: 'tell me about Notion' (may return no tools if Notion not in manifest) ==="
curl -s -X POST "${RUNNER_URL}/agents/$AGENT_NAME/$INSTANCE/a2a" \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message.sendStream",
    "params": {
      "message": {
        "messageId": "verify-2",
        "role": "user",
        "parts": [{"text": "tell me about Notion"}]
      }
    },
    "id": "verify-tool-discovery-2"
  }' | head -100

echo ""
echo "Done. Check output above for tool list (support/calculate, or 'No tools found')."
