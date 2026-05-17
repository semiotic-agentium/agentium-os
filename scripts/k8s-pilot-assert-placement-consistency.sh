#!/usr/bin/env bash
# Placement-consistency assertion for the Kubernetes pilot rehearsal. See `--help`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/k8s-pilot-common.sh
source "${SCRIPT_DIR}/lib/k8s-pilot-common.sh"

usage() {
  cat <<'EOF'
Cluster-health assertion for the Kubernetes pilot rehearsal.

Hits `GET /cluster/agents` (operator-authenticated) and exits non-zero
if any of these invariants is broken:

  - I1: every runner in `runners[]` is `reachable: true`
  - I2: no runner carries an orphan-placement error
  - I3: no agent row has `version_skew: true`
  - I4: every runner's `last_heartbeat_ms` is within the freshness
        threshold (default 30s) — catches slow/throttled heartbeats
        before they cross the 90s placement TTL and become orphans

The intent is to fail fast in the demo-rehearsal flow when the
cluster's placement table has drifted from the live `/agents` view
or a runner's heartbeat cadence has degraded.

The cluster must be installed in cluster mode (multi-runner); standalone
runners return 404 on /cluster/agents.

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
  --heartbeat-threshold-ms <ms>
                       Max permitted heartbeat lag per runner (default 30000).
                       Fails I4 if any runner's last_heartbeat_ms is older
                       than this from now.
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
HEARTBEAT_THRESHOLD_MS=30000

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)      NAMESPACE="$2"; shift 2 ;;
    --url)            RUNNER_URL="$2"; shift 2 ;;
    --secret)         SECRET_NAME="$2"; shift 2 ;;
    --secret-key)     SECRET_KEY="$2"; shift 2 ;;
    --port-forward)   DO_PORT_FORWARD=1; shift ;;
    --service)        API_SERVICE="$2"; shift 2 ;;
    --local-port)     LOCAL_PORT="$2"; shift 2 ;;
    --heartbeat-threshold-ms) HEARTBEAT_THRESHOLD_MS="$2"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *)                fail "unknown argument: $1" 1 ;;
  esac
done

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
resolve_runner_token "$NAMESPACE" "$SECRET_NAME" "$SECRET_KEY"

if [[ "$DO_PORT_FORWARD" -eq 1 ]]; then
  log "opening port-forward to svc/$API_SERVICE on localhost:$LOCAL_PORT"
  precheck_local_port_unbound "$LOCAL_PORT"
  PF_LOG="$(mktemp)"
  kubectl -n "$NAMESPACE" port-forward "svc/$API_SERVICE" "$LOCAL_PORT:$API_PORT" \
    >"$PF_LOG" 2>&1 &
  PF_PID=$!
  RUNNER_URL="http://localhost:$LOCAL_PORT"
  wait_pilot_port_forward_ready "$PF_PID" "$LOCAL_PORT" "$PF_LOG"
fi

log "fetching /cluster/agents"
RESPONSE_FILE="$(mktemp)"
http_code="$(fetch_cluster_agents "$RUNNER_URL" "$RUNNER_TOKEN" "$RESPONSE_FILE")"

if [[ "$http_code" != "200" ]]; then
  body="$(cat "$RESPONSE_FILE" 2>/dev/null || true)"
  fail "GET /cluster/agents returned HTTP $http_code (expected 200). Body: $body" 1
fi

log "asserting cluster-wide consistency"
assert_placement_consistency "$RESPONSE_FILE"
assert_heartbeat_freshness "$RESPONSE_FILE" "$HEARTBEAT_THRESHOLD_MS"

runners_n="$(jq '.runners | length' "$RESPONSE_FILE")"
agents_n="$(jq '.agents | length' "$RESPONSE_FILE")"
log "OK — $runners_n runner(s), $agents_n agent row(s), no skew, heartbeats fresh (<${HEARTBEAT_THRESHOLD_MS}ms)"
