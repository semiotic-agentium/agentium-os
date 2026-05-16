#!/usr/bin/env bash
# Placement-consistency assertion for the Kubernetes pilot rehearsal. See `--help`.

set -euo pipefail

usage() {
  cat <<'EOF'
Placement-consistency assertion for the Kubernetes pilot rehearsal.

Hits `GET /cluster/agents` (operator-authenticated) and exits non-zero
if any of these invariants is broken:

  - every runner in `runners[]` is `reachable: true`
  - no runner carries an orphan-placement error
  - no agent row has `version_skew: true`

The intent is to fail fast in the demo-rehearsal flow when the
cluster's placement table has drifted from the live `/agents` view.

Usage:
  bash scripts/k8s-pilot-assert-placement-consistency.sh [options]

Options:
  --namespace <ns>     Kubernetes namespace (default: agentium)
  --url <base-url>     Runner base URL (default: http://localhost:18080)
  --secret <name>      Runner token secret name (default: runner-token)
  --secret-key <key>   Key within the runner token secret (default: token)
  --port-forward       Open a kubectl port-forward to the API service for
                       the lifetime of the script and close it on exit.
                       Overrides --url to http://localhost:<local-port>.
  --service <name>     API service name for --port-forward
                       (default: agentium-agentium-os-runner-api)
  --local-port <port>  Local port for --port-forward (default: 18080).
  -h, --help           Show this message and exit

Environment:
  RUNNER_TOKEN              If set, used directly. Otherwise the script reads
                            it from the named Kubernetes secret.

Exit codes:
  0  cluster-wide view is internally consistent
  1  precondition or transport failure
  2  placement skew detected
EOF
}

NAMESPACE="agentium"
RUNNER_URL="http://localhost:18080"
SECRET_NAME="runner-token"
SECRET_KEY="token"
API_SERVICE="agentium-agentium-os-runner-api"
API_PORT=18080
LOCAL_PORT=18080
DO_PORT_FORWARD=0
PF_PID=""
PF_LOG=""
RESPONSE_FILE=""

log()  { printf '==> %s\n' "$*"; }
fail() { printf '  x %s\n' "$1" >&2; exit "${2:-1}"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)      NAMESPACE="$2"; shift 2 ;;
    --url)            RUNNER_URL="$2"; shift 2 ;;
    --secret)         SECRET_NAME="$2"; shift 2 ;;
    --secret-key)     SECRET_KEY="$2"; shift 2 ;;
    --port-forward)   DO_PORT_FORWARD=1; shift ;;
    --service)        API_SERVICE="$2"; shift 2 ;;
    --local-port)     LOCAL_PORT="$2"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *)                fail "unknown argument: $1" 1 ;;
  esac
done

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1 (install it and re-run)" 1
}

require_cmd kubectl
require_cmd curl
require_cmd jq

cleanup() {
  local code=$?
  if [[ -n "$PF_PID" ]] && kill -0 "$PF_PID" 2>/dev/null; then
    kill "$PF_PID" 2>/dev/null || true
    wait "$PF_PID" 2>/dev/null || true
  fi
  [[ -n "$PF_LOG" && -f "$PF_LOG" ]] && rm -f "$PF_LOG"
  [[ -n "$RESPONSE_FILE" && -f "$RESPONSE_FILE" ]] && rm -f "$RESPONSE_FILE"
  exit "$code"
}
trap cleanup EXIT INT TERM

log "resolving runner token"
if [[ -z "${RUNNER_TOKEN:-}" ]]; then
  if ! RUNNER_TOKEN="$(kubectl -n "$NAMESPACE" get secret "$SECRET_NAME" \
        -o "jsonpath={.data.$SECRET_KEY}" 2>/dev/null | base64 -d)"; then
    fail "could not read secret ${NAMESPACE}/${SECRET_NAME} key=${SECRET_KEY}. Set RUNNER_TOKEN or pass --secret." 1
  fi
  if [[ -z "$RUNNER_TOKEN" ]]; then
    fail "secret ${NAMESPACE}/${SECRET_NAME} key=${SECRET_KEY} is empty" 1
  fi
fi
export RUNNER_TOKEN

if [[ "$DO_PORT_FORWARD" -eq 1 ]]; then
  log "opening port-forward to svc/$API_SERVICE on localhost:$LOCAL_PORT"
  if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:$LOCAL_PORT/healthz" 2>/dev/null; then
    fail "localhost:$LOCAL_PORT already responds to /healthz — another process is bound here. Stop it or re-run with --local-port <port>." 1
  fi
  PF_LOG="$(mktemp)"
  kubectl -n "$NAMESPACE" port-forward "svc/$API_SERVICE" "$LOCAL_PORT:$API_PORT" \
    >"$PF_LOG" 2>&1 &
  PF_PID=$!
  RUNNER_URL="http://localhost:$LOCAL_PORT"
  pf_ready=0
  for _ in $(seq 1 60); do
    if ! kill -0 "$PF_PID" 2>/dev/null; then
      fail "kubectl port-forward exited before becoming ready: $(cat "$PF_LOG")" 1
    fi
    if curl -sf -o /dev/null --connect-timeout 1 "$RUNNER_URL/healthz" 2>/dev/null; then
      pf_ready=1
      break
    fi
    sleep 0.5
  done
  if [[ "$pf_ready" -ne 1 ]]; then
    fail "port-forward did not become ready within 30s: $(cat "$PF_LOG")" 1
  fi
fi

log "fetching /cluster/agents"
RESPONSE_FILE="$(mktemp)"
# `--retry 2 --retry-connrefused --retry-delay 1` absorbs a transient TCP blip
# during the peer fan-out (each runner has a 5s timeout inside the handler) so
# a single network hiccup does not fail the rehearsal.
http_code="$(curl -sS -o "$RESPONSE_FILE" -w '%{http_code}' \
  --retry 2 --retry-connrefused --retry-delay 1 \
  -H "X-Runner-Token: $RUNNER_TOKEN" \
  "$RUNNER_URL/cluster/agents" || true)"

if [[ "$http_code" != "200" ]]; then
  body="$(cat "$RESPONSE_FILE" 2>/dev/null || true)"
  fail "GET /cluster/agents returned HTTP $http_code (expected 200). Body: $body" 1
fi

log "asserting cluster-wide consistency"

# I1 — every runner reachable.
unreachable_count="$(jq '[.runners[] | select(.reachable == false)] | length' "$RESPONSE_FILE")"
if [[ "$unreachable_count" -gt 0 ]]; then
  detail="$(jq -r '[.runners[] | select(.reachable == false) | "\(.runner_id) (\(.error))"] | join(", ")' "$RESPONSE_FILE")"
  fail "[FAIL I1] $unreachable_count runner(s) unreachable from /cluster/agents fan-out: $detail" 2
fi

# I2 — no orphan-placement entries (runner_id in cluster_agent_placements but
# not in cluster_runners). The /cluster/agents handler labels these in the
# `error` field with this substring; matching it keeps this script wedged to
# the server's contract rather than re-implementing the detection here.
orphan_count="$(jq '[.runners[] | select(.error and (.error | contains("orphan placement")))] | length' "$RESPONSE_FILE")"
if [[ "$orphan_count" -gt 0 ]]; then
  detail="$(jq -r '[.runners[] | select(.error and (.error | contains("orphan placement"))) | .runner_id] | join(", ")' "$RESPONSE_FILE")"
  fail "[FAIL I2] $orphan_count orphan placement(s) found (placement-table runner_id not in cluster_runners): $detail" 2
fi

# I3 — no version skew across placements of the same agent.
skew_count="$(jq '[.agents[] | select(.version_skew == true)] | length' "$RESPONSE_FILE")"
if [[ "$skew_count" -gt 0 ]]; then
  detail="$(jq -r '[.agents[] | select(.version_skew == true) | "\(.agent_package)/\(.agent_instance_id)"] | join(", ")' "$RESPONSE_FILE")"
  fail "[FAIL I3] $skew_count agent(s) with version skew across placements: $detail" 2
fi

runners_n="$(jq '.runners | length' "$RESPONSE_FILE")"
agents_n="$(jq '.agents | length' "$RESPONSE_FILE")"
log "OK — $runners_n runner(s), $agents_n agent row(s), no skew"
