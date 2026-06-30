#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Exploratory/CI smoke: temp-dir bootstrap → patch deterministic echo → publish → deploy → A2A.
# No committed agent fixture — everything under VERIFY_TMP is discarded after the run.
#
# Usage: ./scripts/verify-bootstrap-eval.sh
# Env: BIND (default 127.0.0.1:18080), same VERIFY_* vars as verify-agentium-console.sh

set -euo pipefail
cd "$(dirname "$0")/.."

BIND="${BIND:-127.0.0.1:18080}"
RUNNER_URL="http://${BIND}"
REPOSITORY_URL="${RUNNER_URL}/repository"

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUILDER_BIN="${VERIFY_BUILDER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-builder}"
RUNNER_BIN="${VERIFY_RUNNER_BIN:-$CARGO_TARGET_DIR/debug/baml-agent-runner}"

VERIFY_TMP="${VERIFY_RUNNER_TMPDIR:-$(mktemp -d "${TMPDIR:-/tmp}/bootstrap-eval-XXXXXX")}"
STATE_DIR="${VERIFY_RUNNER_STATE_DIR:-$VERIFY_TMP/state}"
REPO_DIR="${VERIFY_RUNNER_REPOSITORY_DIR:-$VERIFY_TMP/repository}"
BOOT_DIR="${VERIFY_TMP}/bootstrap-agent"
mkdir -p "$STATE_DIR" "$REPO_DIR"

RUNNER_PID=""
cleanup() {
  if [[ -n "$RUNNER_PID" ]]; then kill "$RUNNER_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

echo "=== Bootstrap + eval verification (ephemeral agent) ==="

ensure_runner() {
  if curl -s -o /dev/null -w "%{http_code}" --connect-timeout 2 "${RUNNER_URL}/agents" 2>/dev/null | grep -q 200; then
    echo "Runner already up on $BIND"
    return
  fi
  echo "Starting runner on $BIND ..."
  cargo build -p baml-agent-runner -q
  RUST_LOG="${RUST_LOG:-error}" \
    "$RUNNER_BIN" \
    --serve-http "$BIND" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$STATE_DIR" \
    --repository-dir "$REPO_DIR" \
    &
  RUNNER_PID=$!
  for _ in $(seq 1 60); do
    if curl -sf "${RUNNER_URL}/openapi.json" >/dev/null 2>&1; then return; fi
    sleep 1
  done
  echo "Runner failed to start." >&2
  exit 1
}

publish_deploy_dir() {
  local agent_dir="$1"
  cargo build -p baml-rt-builder -q --bin baml-agent-builder
  "$BUILDER_BIN" publish \
    --agent-dir "$agent_dir" \
    --repository-url "$REPOSITORY_URL" \
    --deploy-url "$RUNNER_URL" >/dev/null
}

agent_package_name() {
  jq -r .name "${BOOT_DIR}/manifest.json"
}

eval_a2a() {
  local package="$1"
  local user_text="$2"
  local expected_substr="$3"
  local millis msg_id corr_id body out
  millis=$(python3 -c 'import time; print(int(time.time()*1000))')
  msg_id="eval-msg-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  body="$(jq -nc \
    --arg text "$user_text" \
    --arg id "$msg_id" \
    --arg corr "$corr_id" \
    '{jsonrpc:"2.0",id:$corr,method:"message.sendStream",params:{message:{messageId:$id,role:"user",parts:[{text:$text}]}}}')"
  out="$(curl -sS --max-time 120 -X POST "${RUNNER_URL}/agents/${package}/default/a2a" \
    -H "Content-Type: application/json" \
    -d "$body")"
  if echo "$out" | grep -q "$expected_substr"; then
    echo "  OK A2A eval ${package}: found ${expected_substr}"
    return 0
  fi
  echo "FAIL A2A eval ${package}: expected substring ${expected_substr}" >&2
  echo "$out" | tail -c 2000 >&2
  return 1
}

patch_deterministic_index() {
  cat >"${BOOT_DIR}/src/index.ts" <<'EOF'
/// <reference path="./baml-runtime.d.ts" />
__chat_register({
  run: async (ctx) => {
    const text = (ctx.text || "").trim();
    return { message: `eval:pass:${text || "ping"}` };
  },
});
export {};
EOF
}

ensure_runner

echo "Bootstrap → publish → deploy → A2A eval (temp dir: ${BOOT_DIR})"
rm -rf "$BOOT_DIR"
mkdir -p "$BOOT_DIR"
cargo build -p baml-rt-builder -q --bin baml-agent-builder
"$BUILDER_BIN" bootstrap "$BOOT_DIR" \
  --name "Bootstrap Eval Smoke" \
  --description "Ephemeral bootstrap eval smoke" \
  --no-tools
patch_deterministic_index
PKG="$(agent_package_name)"
publish_deploy_dir "$BOOT_DIR"
eval_a2a "$PKG" "bootstrap-probe" "eval:pass:bootstrap-probe"

echo ""
echo "Bootstrap + eval verification passed (agent was not written to the repo)."
