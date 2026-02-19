#!/usr/bin/env bash
# Ad hoc verification: build tool-discovery-demo package, start runner, send a message
# and expect a response that used system/discover_tools (e.g. mentions "support/calculate" or "Notion").
# Usage: ./scripts/verify-tool-discovery.sh [--build]
# Requires: OPENROUTER_API_KEY in env or .env for LLM (ChooseDiscoverToolsQuery).

set -e
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:8081}"
PKG="/tmp/verify-tool-discovery-demo.tar.gz"
AGENT_DIR="tests/fixtures/agents/tool-discovery-demo"

do_build=false
for arg in "$@"; do
  [[ "$arg" == "--build" ]] && do_build=true
done

if [[ "$do_build" == "true" ]] || [[ ! -f "$PKG" ]]; then
  echo "Building tool-discovery-demo package..."
  cargo run -p baml-rt-builder --bin baml-agent-builder -- package \
    --agent-dir "$AGENT_DIR" --output "$PKG" --skip-lint
  echo "Package built: $PKG"
fi

RUNNER_PID=""
if ! curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "http://$BIND/agents" 2>/dev/null | grep -q 200; then
  echo "Starting runner on $BIND with package $PKG..."
  cargo run -p baml-agent-runner -- "$PKG" --serve-http "$BIND" &
  RUNNER_PID=$!
  trap 'kill $RUNNER_PID 2>/dev/null || true' EXIT
  echo "Waiting for server..."
  for i in $(seq 1 30); do
    if curl -s -o /dev/null -w "%{http_code}" "http://$BIND/agents" 2>/dev/null | grep -q 200; then
      break
    fi
    [[ -n "$RUNNER_PID" ]] && ! kill -0 $RUNNER_PID 2>/dev/null && exit 1
    sleep 1
  done
fi

# Resolve agent name from manifest (tool-discovery-demo)
AGENT_NAME="tool-discovery-demo"
INSTANCE="default"

echo ""
echo "=== POST message: 'what tools do you have for calculate?' ==="
curl -s -X POST "http://$BIND/agents/$AGENT_NAME/$INSTANCE/a2a" \
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
curl -s -X POST "http://$BIND/agents/$AGENT_NAME/$INSTANCE/a2a" \
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
