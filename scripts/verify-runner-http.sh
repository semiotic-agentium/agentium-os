#!/usr/bin/env bash
# Build two fixture agent packages, start baml-agent-runner with --serve-http,
# then verify with curl: GET /agents, GET /openapi.json, POST /agents/.../a2a.
# Usage: ./scripts/verify-runner-http.sh [--build]
#   --build: rebuild fixture packages even if .tar.gz exist (default: use existing)
# GET /agents works without provenance; full A2A uses SurrealDB (default provenance store in cwd).

set -e
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:8080}"
PKG1="/tmp/runner-verify-stream-baml-tool.tar.gz"
PKG2="/tmp/runner-verify-stream-js-tool.tar.gz"

do_build=false
for arg in "$@"; do
  if [[ "$arg" == "--build" ]]; then do_build=true; fi
done

if [[ "$do_build" == "true" ]] || [[ ! -f "$PKG1" ]] || [[ ! -f "$PKG2" ]]; then
  echo "Building fixture packages..."
  cargo run -p baml-rt-builder --bin baml-agent-builder -- package \
    --agent-dir tests/fixtures/agents/stream-baml-tool --output "$PKG1" --skip-lint
  cargo run -p baml-rt-builder --bin baml-agent-builder -- package \
    --agent-dir tests/fixtures/agents/stream-js-tool --output "$PKG2" --skip-lint
  echo "Packages built."
fi

RUNNER_PID=""
if ! curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "http://$BIND/agents" 2>/dev/null | grep -q 200; then
  echo "Starting runner on $BIND..."
  cargo run -p baml-agent-runner -- "$PKG1" "$PKG2" \
    --serve-http "$BIND" &
  RUNNER_PID=$!
  trap 'kill $RUNNER_PID 2>/dev/null || true' EXIT
  echo "Waiting for server..."
  for i in $(seq 1 60); do
    if curl -s -o /dev/null -w "%{http_code}" "http://$BIND/agents" 2>/dev/null | grep -q 200; then
      break
    fi
    if [[ -n "$RUNNER_PID" ]] && ! kill -0 $RUNNER_PID 2>/dev/null; then
      echo "Runner exited early."
      exit 1
    fi
    sleep 1
  done
else
  echo "Server already responding on $BIND, using it."
fi

JQ=jq
command -v jq >/dev/null 2>&1 || JQ=cat

echo ""
echo "=== GET /agents ==="
curl -s "http://$BIND/agents" | $JQ .

echo ""
echo "=== GET /openapi.json (info + paths keys) ==="
if command -v jq >/dev/null 2>&1; then
  curl -s "http://$BIND/openapi.json" | jq '{ openapi, info, paths: (.paths | keys) }'
else
  curl -s "http://$BIND/openapi.json" | head -30
fi

echo ""
echo "=== POST /agents/stream-baml-tool/default/a2a (tasks.list) ==="
curl -s -X POST "http://$BIND/agents/stream-baml-tool/default/a2a" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | $JQ .

echo ""
echo "=== POST unknown agent (expect 404) ==="
curl -s -w "\nHTTP %{http_code}" -X POST "http://$BIND/agents/none/default/a2a" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | tail -5

echo ""
echo "=== SSE A2A (tasks.list) ==="
curl -s -N -X POST "http://$BIND/agents/stream-baml-tool/default/a2a/sse" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | head -20
echo ""
echo "(SSE stream above; look for data: lines with JSON-RPC result)"

echo ""
if [[ -n "$RUNNER_PID" ]]; then
  echo "Done. Runner PID $RUNNER_PID (will be killed on exit)."
else
  echo "Done. Runner was already running; left it up."
fi
