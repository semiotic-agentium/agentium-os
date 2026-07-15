#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Collect tier-3 gate E2E evidence artifacts into e2e-evidence/gate-tier3/
set -euo pipefail

ROOT="$(git -C "${BASH_SOURCE%/*}/.." rev-parse --show-toplevel)"
OUT="${ROOT}/e2e-evidence/gate-tier3"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
RUNNER="${RUNNER_BASE_URL:-http://127.0.0.1:18080}"
mkdir -p "${OUT}"

log() { echo "[gate-evidence ${TS}] $*" | tee -a "${OUT}/run.log"; }

log "=== 1. Rust A2A gate authorization E2E ==="
cd "${ROOT}"
cargo test -p baml-rt-a2a --test gate_authorization_a2a_e2e_test \
  test_gate_authorization_tier3_suspend_and_approve_resume -- --nocapture \
  2>&1 | tee "${OUT}/rust-a2a-e2e-${TS}.log"

log "=== 2. Web gate UI unit tests ==="
cd "${ROOT}/web"
npm test -- --run gateAuthorization GateAuthorization 2>&1 | tee "${OUT}/web-vitest-${TS}.log"

log "=== 3. Runner HTTP + semiotic config API ==="
if curl -sf "${RUNNER}/healthz" >/dev/null 2>&1; then
  curl -s "${RUNNER}/healthz" | tee "${OUT}/healthz-${TS}.json"
  curl -s "${RUNNER}/config/semiotic" | tee "${OUT}/semiotic-get-before-${TS}.json"
  curl -s -X PUT "${RUNNER}/config/semiotic" \
    -H 'Content-Type: application/json' \
    -d '{
      "enabled": true,
      "mode": "enforce",
      "enforceMinTier": 2,
      "requirePostconditionsT3": true,
      "strictCitationAnchors": true,
      "overrides": { "agent": {} }
    }' | tee "${OUT}/semiotic-put-${TS}.json"
  curl -s "${RUNNER}/config/semiotic" | tee "${OUT}/semiotic-get-after-${TS}.json"
  curl -s "${RUNNER}/repository/agents" | tee "${OUT}/repository-agents-${TS}.json"
  curl -s "${RUNNER}/deployments" | tee "${OUT}/deployments-${TS}.json"
else
  log "SKIP runner API (not reachable at ${RUNNER})"
fi

log "Evidence written under ${OUT}"
