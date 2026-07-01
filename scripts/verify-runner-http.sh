#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Start baml-agent-runner with --serve-http and embedded repository, publish+deploy
# two fixture agents, then verify with curl: GET /agents, GET /openapi.json, POST /agents/.../a2a.
# Usage: ./scripts/verify-runner-http.sh [--build]
#   --build: always publish+deploy (default: publish+deploy when starting a new runner)
# GET /agents works without provenance; full A2A uses SurrealDB (default provenance store in cwd).

set -e
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:18080}"
RUNNER_URL="http://${BIND}"
REPOSITORY_URL="${RUNNER_URL}/repository"
FIXTURE1="tests/fixtures/agents/stream-baml-tool"
FIXTURE2="tests/fixtures/agents/stream-js-tool"

VERIFY_TMP="${VERIFY_RUNNER_TMPDIR:-$(mktemp -d "${TMPDIR:-/tmp}/runner-verify-XXXXXX")}"
STATE_DIR="${VERIFY_RUNNER_STATE_DIR:-$VERIFY_TMP/state}"
REPO_DIR="${VERIFY_RUNNER_REPOSITORY_DIR:-$VERIFY_TMP/repository}"
mkdir -p "$STATE_DIR" "$REPO_DIR"

do_build=false
for arg in "$@"; do
  if [[ "$arg" == "--build" ]]; then do_build=true; fi
done

RUNNER_PID=""
if ! curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "${RUNNER_URL}/agents" 2>/dev/null | grep -q 200; then
  echo "Starting runner on $BIND (repository + state under $VERIFY_TMP)..."
  cargo build -p baml-agent-runner -q
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  RUNNER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium"
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
  for i in $(seq 1 60); do
    if curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
      break
    fi
    if [[ -n "$RUNNER_PID" ]] && ! kill -0 "$RUNNER_PID" 2>/dev/null; then
      echo "Runner exited early."
      exit 1
    fi
    sleep 1
  done
  if ! curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
    echo "Runner failed to expose OpenAPI at ${RUNNER_URL}/openapi.json" >&2
    exit 1
  fi

  cargo build -p baml-rt-builder -q --bin baml-agent-builder
  BUILDER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium"
  echo "Publishing fixture agents..."
  "$BUILDER_BIN" publish --agent-dir "$FIXTURE1" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
  "$BUILDER_BIN" publish --agent-dir "$FIXTURE2" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
elif [[ "$do_build" == "true" ]]; then
  echo "Server already responding on $BIND; publishing fixtures anyway (--build)."
  cargo build -p baml-rt-builder -q --bin baml-agent-builder
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  BUILDER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium"
  "$BUILDER_BIN" publish --agent-dir "$FIXTURE1" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
  "$BUILDER_BIN" publish --agent-dir "$FIXTURE2" --repository-url "$REPOSITORY_URL" --deploy-url "$RUNNER_URL"
else
  echo "Server already responding on $BIND, using it (skip publish; set VERIFY_RUNNER_* or use --build to republish)."
fi

JQ=jq
command -v jq >/dev/null 2>&1 || JQ=cat

echo ""
echo "=== GET /agents ==="
curl -s "${RUNNER_URL}/agents" | $JQ .

echo ""
echo "=== GET /openapi.json (info + paths keys) ==="
if command -v jq >/dev/null 2>&1; then
  curl -s "${RUNNER_URL}/openapi.json" | jq '{ openapi, info, paths: (.paths | keys) }'
else
  curl -s "${RUNNER_URL}/openapi.json" | head -30
fi

echo ""
echo "=== POST /agents/stream-baml-tool/default/a2a (tasks.list) ==="
curl -s -X POST "${RUNNER_URL}/agents/stream-baml-tool/default/a2a" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | $JQ .

echo ""
echo "=== POST unknown agent (expect 404) ==="
curl -s -w "\nHTTP %{http_code}" -X POST "${RUNNER_URL}/agents/none/default/a2a" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | tail -5

echo ""
echo "=== SSE A2A (tasks.list) ==="
curl -s -N -X POST "${RUNNER_URL}/agents/stream-baml-tool/default/a2a/sse" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}' | head -20
echo ""
echo "(SSE stream above; look for data: lines with JSON-RPC result)"

echo ""
if [[ -n "$RUNNER_PID" ]]; then
  echo "Done. Runner PID $RUNNER_PID (will be killed on exit). Temp data: $VERIFY_TMP"
else
  echo "Done. Runner was already running; left it up."
fi
