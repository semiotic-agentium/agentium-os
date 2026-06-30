#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Verify Agentium Console devx: web tests/build + runner API routes the console uses.
# Usage: ./scripts/verify-agentium-console.sh [--skip-web]
# Env: BIND (default 127.0.0.1:18080), same VERIFY_* vars as verify-runner-http.sh

set -euo pipefail
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:18080}"
RUNNER_URL="http://${BIND}"
REPOSITORY_URL="${RUNNER_URL}/repository"
FIXTURE="${VERIFY_CONSOLE_FIXTURE:-tests/fixtures/agents/task-lifecycle-demo}"

skip_web=false
for arg in "$@"; do
  if [[ "$arg" == "--skip-web" ]]; then skip_web=true; fi
done

echo "=== Agentium Console verification ==="

if [[ "$skip_web" != "true" ]]; then
  echo "[1/3] Web: vitest + production build"
  (cd web && npm run test && npm run build)
else
  echo "[1/3] Web: skipped (--skip-web)"
fi

VERIFY_TMP="${VERIFY_RUNNER_TMPDIR:-$(mktemp -d "${TMPDIR:-/tmp}/console-verify-XXXXXX")}"
STATE_DIR="${VERIFY_RUNNER_STATE_DIR:-$VERIFY_TMP/state}"
REPO_DIR="${VERIFY_RUNNER_REPOSITORY_DIR:-$VERIFY_TMP/repository}"
mkdir -p "$STATE_DIR" "$REPO_DIR"

RUNNER_PID=""
cleanup() {
  if [[ -n "$RUNNER_PID" ]]; then
    kill "$RUNNER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if ! curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "${RUNNER_URL}/agents" 2>/dev/null | grep -q 200; then
  echo "[2/3] Starting runner on $BIND ..."
  cargo build -p baml-agent-runner -q
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
  RUNNER_BIN="${VERIFY_RUNNER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-runner}"
  RUST_LOG="${RUST_LOG:-error}" \
    "$RUNNER_BIN" \
    --serve-http "$BIND" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPO_DIR" \
    &
  RUNNER_PID=$!
  for _ in $(seq 1 60); do
    if curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then break; fi
    if [[ -n "$RUNNER_PID" ]] && ! kill -0 "$RUNNER_PID" 2>/dev/null; then
      echo "Runner exited early." >&2
      exit 1
    fi
    sleep 1
  done
  if ! curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then
    echo "Runner failed to start." >&2
    exit 1
  fi
else
  echo "[2/3] Runner already up on $BIND"
fi

echo "[3/3] Console API routes + publish/deploy (Agents view parity)"
CONSOLE_PATHS=(
  "/agents"
  "/healthz"
  "/deployments"
  "/message-shapes"
  "/openapi.json"
)
for path in "${CONSOLE_PATHS[@]}"; do
  code="$(curl -s -o /dev/null -w "%{http_code}" "${RUNNER_URL}${path}")"
  if [[ "$code" != "200" && "$path" != "/healthz" ]]; then
    echo "FAIL ${path} returned HTTP ${code}" >&2
    exit 1
  fi
  # healthz may be 200 or 404 depending on router wiring
  if [[ "$path" == "/healthz" && "$code" != "200" && "$code" != "404" ]]; then
    echo "FAIL ${path} returned HTTP ${code}" >&2
    exit 1
  fi
  echo "  OK ${path} (${code})"
done

if [[ ! -d "$FIXTURE" ]]; then
  echo "Fixture missing: $FIXTURE" >&2
  exit 1
fi

cargo build -p baml-rt-builder -q --bin baml-agent-builder
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUILDER_BIN="${VERIFY_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"
echo "  Publishing ${FIXTURE} (same contract as Agents folder upload) ..."
PUBLISH_OUT="$("$BUILDER_BIN" publish \
  --agent-dir "$FIXTURE" \
  --repository-url "$REPOSITORY_URL" \
  --deploy-url "$RUNNER_URL" 2>&1)"
echo "$PUBLISH_OUT" | tail -3

AGENT_NAME="$(basename "$FIXTURE")"
if ! curl -sf "${RUNNER_URL}/agents" | grep -q "$AGENT_NAME"; then
  echo "FAIL: agent ${AGENT_NAME} not listed after publish+deploy" >&2
  exit 1
fi
echo "  OK agent ${AGENT_NAME} discoverable via GET /agents"

echo ""
echo "Agentium Console verification passed."
