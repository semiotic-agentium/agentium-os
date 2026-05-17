#!/usr/bin/env bash
# Kubernetes pilot first-run smoke flow. See `--help` for usage.
# Packages steps 6–8 of docs/k8s-pilot-operator-guide.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/k8s-pilot-common.sh
source "${SCRIPT_DIR}/lib/k8s-pilot-common.sh"

usage() {
  cat <<'EOF'
Kubernetes pilot first-run smoke flow.

Usage:
  bash scripts/k8s-pilot-smoke.sh [options]

Options:
  --namespace <ns>     Kubernetes namespace (default: agentium)
  --release <name>     Helm release / instance label (default: agentium).
                       Used to discover the runner StatefulSet so the
                       install publishes the fixture to every runner
                       pod's local repository.
  --url <base-url>     Runner base URL (default: http://localhost:18080)
  --secret <name>      Runner token secret name (default: runner-token)
  --secret-key <key>   Key within the runner token secret (default: token)
  --port-forward       Open a kubectl port-forward to the API service for
                       the lifetime of the script and close it on exit.
                       Overrides --url to http://localhost:<local-port>.
  --service <name>     API service name for --port-forward
                       (default: agentium-agentium-os-runner-api)
  --local-port <port>  Local port for --port-forward (default: 18080).
                       Pick a different port if localhost:18080 is already
                       in use (e.g. by a local dev runner).
  --keep-deployed      Do not undeploy the fixture at the end
  -h, --help           Show this message and exit

Environment:
  RUNNER_TOKEN              If set, used directly. Otherwise the script reads
                            it from the named Kubernetes secret.
  K8S_PILOT_PF_LOG_DIR      Directory for the kubectl port-forward log. When
                            set, the log is written to <dir>/port-forward.log
                            and survives this script's cleanup. When unset,
                            the log goes to a tempfile that is removed on
                            exit. scripts/verify-k8s-pilot-package.sh sets
                            this so the log lands beside its other artifacts.

Exit codes:
  0  smoke passed
  1  precondition or transport failure
  2  publish/deploy failure
  3  dispatch verification failure
EOF
}

NAMESPACE="agentium"
RELEASE_NAME="agentium"
RUNNER_URL="http://localhost:18080"
SECRET_NAME="runner-token"
SECRET_KEY="token"
API_SERVICE="agentium-agentium-os-runner-api"
API_PORT=18080
LOCAL_PORT=18080
DO_PORT_FORWARD=0
KEEP_DEPLOYED=0
PF_PID=""
PF_LOG=""
CLUSTER_AGENTS_FILE=""
# Per-pod publish port-forwards (one ephemeral PF per runner pod). Tracked
# as parallel arrays of "pid|log-path" so the cleanup trap can tear them
# down in lockstep.
declare -a POD_PF_PIDS=()
declare -a POD_PF_LOGS=()
PUBLISH_PORT_BASE=18181

FIXTURE_PATH="tests/fixtures/agents/dispatch-echo"
FIXTURE_PKG="dispatch-echo"
FIXTURE_INSTANCE="default"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)      NAMESPACE="$2"; shift 2 ;;
    --release)        RELEASE_NAME="$2"; shift 2 ;;
    --url)            RUNNER_URL="$2"; shift 2 ;;
    --secret)         SECRET_NAME="$2"; shift 2 ;;
    --secret-key)     SECRET_KEY="$2"; shift 2 ;;
    --port-forward)   DO_PORT_FORWARD=1; shift ;;
    --service)        API_SERVICE="$2"; shift 2 ;;
    --local-port)     LOCAL_PORT="$2"; shift 2 ;;
    --keep-deployed)  KEEP_DEPLOYED=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *)                fail "unknown argument: $1" 1 ;;
  esac
done

require_cmd kubectl
require_cmd curl
require_cmd jq
require_cmd cargo

if [[ ! -d "$FIXTURE_PATH" ]]; then
  fail "fixture directory not found: $FIXTURE_PATH (run from repository root)" 1
fi

cleanup() {
  local code=$?
  if [[ -n "$PF_PID" ]] && kill -0 "$PF_PID" 2>/dev/null; then
    kill "$PF_PID" 2>/dev/null || true
    wait "$PF_PID" 2>/dev/null || true
  fi
  if [[ -n "$PF_LOG" && -f "$PF_LOG" && -z "${K8S_PILOT_PF_LOG_DIR:-}" ]]; then
    rm -f "$PF_LOG"
  fi
  local i pid log
  for i in "${!POD_PF_PIDS[@]}"; do
    pid="${POD_PF_PIDS[$i]}"
    log="${POD_PF_LOGS[$i]}"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
    if [[ -n "$log" && -f "$log" && -z "${K8S_PILOT_PF_LOG_DIR:-}" ]]; then
      rm -f "$log"
    fi
  done
  [[ -n "$CLUSTER_AGENTS_FILE" && -f "$CLUSTER_AGENTS_FILE" ]] && rm -f "$CLUSTER_AGENTS_FILE"
  exit "$code"
}
trap cleanup EXIT INT TERM

log "step 1: resolving runner token"
resolve_runner_token "$NAMESPACE" "$SECRET_NAME" "$SECRET_KEY"

healthz_ok=0
if [[ "$DO_PORT_FORWARD" -eq 1 ]]; then
  log "step 2: opening port-forward to svc/$API_SERVICE on localhost:$LOCAL_PORT"
  precheck_local_port_unbound "$LOCAL_PORT"
  if [[ -n "${K8S_PILOT_PF_LOG_DIR:-}" ]]; then
    mkdir -p "$K8S_PILOT_PF_LOG_DIR"
    PF_LOG="${K8S_PILOT_PF_LOG_DIR}/port-forward.log"
    log "    port-forward log persisted at $PF_LOG"
  else
    PF_LOG="$(mktemp)"
  fi
  kubectl -n "$NAMESPACE" port-forward "svc/$API_SERVICE" "$LOCAL_PORT:$API_PORT" \
    >"$PF_LOG" 2>&1 &
  PF_PID=$!
  RUNNER_URL="http://localhost:$LOCAL_PORT"
  wait_pilot_port_forward_ready "$PF_PID" "$LOCAL_PORT" "$PF_LOG"
  healthz_ok=1
fi

log "step 3: probing runner at $RUNNER_URL"
if [[ "$healthz_ok" -ne 1 ]] && ! curl -sf "$RUNNER_URL/healthz" >/dev/null; then
  fail "runner not reachable at $RUNNER_URL/healthz. Install the chart and/or open a port-forward first." 1
fi
if ! curl -sf "$RUNNER_URL/readyz" >/dev/null; then
  fail "runner /readyz is failing. Check pod status and SurrealDB connectivity." 1
fi

log "step 4: discovering runner pods (cluster install needs per-pod publish)"
# Each runner has its own local repository (per-pod SurrealDB-backed
# store). `POST /cluster/deploy` fans out by hash but requires every
# runner to have already published that hash. So the install must
# publish to each pod, then deploy once cluster-wide.
runner_sts="$(kubectl -n "$NAMESPACE" get statefulset \
  -l "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/component=runner" \
  -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)"
if [[ -z "$runner_sts" ]]; then
  fail "no runner StatefulSet found for release '${RELEASE_NAME}' in namespace '${NAMESPACE}'" 1
fi
if [[ "$(printf '%s\n' "$runner_sts" | wc -w)" != "1" ]]; then
  fail "expected exactly one runner StatefulSet, found: $runner_sts" 1
fi
replicas="$(kubectl -n "$NAMESPACE" get statefulset "$runner_sts" \
  -o jsonpath='{.spec.replicas}' 2>/dev/null)"
if [[ -z "$replicas" || "$replicas" -lt 1 ]]; then
  fail "runner StatefulSet ${runner_sts} reports replicas=${replicas:-unknown}" 1
fi
log "    StatefulSet=${runner_sts}, replicas=${replicas}"

log "step 5: publishing $FIXTURE_PKG to every runner pod's local repository"
content_hash=""
for ((i=0; i<replicas; i++)); do
  pod="${runner_sts}-${i}"
  pod_port=$((PUBLISH_PORT_BASE + i))
  precheck_local_port_unbound "$pod_port"
  if [[ -n "${K8S_PILOT_PF_LOG_DIR:-}" ]]; then
    pod_pf_log="${K8S_PILOT_PF_LOG_DIR}/port-forward-${pod}.log"
  else
    pod_pf_log="$(mktemp)"
  fi
  kubectl -n "$NAMESPACE" port-forward "pod/${pod}" "${pod_port}:${API_PORT}" \
    >"$pod_pf_log" 2>&1 &
  pod_pid=$!
  POD_PF_PIDS+=("$pod_pid")
  POD_PF_LOGS+=("$pod_pf_log")
  pod_url="http://localhost:${pod_port}"
  wait_pilot_port_forward_ready "$pod_pid" "$pod_port" "$pod_pf_log"

  publish_output="$(cargo run -q -p cargo-agent-platform -- publish \
    --agent-dir "$FIXTURE_PATH" \
    --repository-url "${pod_url}/repository" \
    --runner-token "$RUNNER_TOKEN" 2>&1)" || {
    printf '%s\n' "$publish_output" >&2
    fail "publish to pod/${pod} failed" 2
  }
  pod_hash="$(printf '%s\n' "$publish_output" | awk '/^[[:space:]]*hash:/ {print $2}' | tail -n 1)"
  if [[ -z "$pod_hash" ]]; then
    printf '%s\n' "$publish_output" >&2
    fail "could not extract publish hash from pod/${pod} output" 2
  fi
  log "    pod/${pod}: hash=${pod_hash}"
  if [[ -z "$content_hash" ]]; then
    content_hash="$pod_hash"
  elif [[ "$content_hash" != "$pod_hash" ]]; then
    fail "hash mismatch between pods: ${content_hash} vs ${pod_hash} on pod/${pod} (publish is content-addressable; same source must yield same hash)" 2
  fi
done

log "step 6: cluster-wide deploy via POST /cluster/deploy"
deploy_body="$(jq -n --arg h "$content_hash" '{hash: $h}')"
deploy_response="$(curl -sf -X POST "${RUNNER_URL}/cluster/deploy" \
  -H "X-Runner-Token: ${RUNNER_TOKEN}" \
  -H 'content-type: application/json' \
  -d "$deploy_body" || true)"
if [[ -z "$deploy_response" ]]; then
  fail "POST /cluster/deploy returned no body — runner may have rejected the request" 2
fi
all_succeeded="$(printf '%s' "$deploy_response" | jq -r '.all_succeeded // false')"
if [[ "$all_succeeded" != "true" ]]; then
  warn "cluster deploy response: $deploy_response"
  fail "POST /cluster/deploy did not report all_succeeded=true" 2
fi
log "    cluster-wide deploy ok for hash=${content_hash}"

log "step 7: verifying cluster-wide consistency via GET /cluster/agents"
CLUSTER_AGENTS_FILE="$(mktemp)"
http_code="$(fetch_cluster_agents "$RUNNER_URL" "$RUNNER_TOKEN" "$CLUSTER_AGENTS_FILE")"
if [[ "$http_code" != "200" ]]; then
  body="$(cat "$CLUSTER_AGENTS_FILE" 2>/dev/null || true)"
  fail "GET /cluster/agents returned HTTP $http_code (expected 200). Body: $body" 3
fi
fixture_row="$(jq --arg pkg "$FIXTURE_PKG" \
  '.agents[] | select(.agent_package == $pkg)' "$CLUSTER_AGENTS_FILE")"
if [[ -z "$fixture_row" ]]; then
  fail "$FIXTURE_PKG not listed on /cluster/agents after deploy" 3
fi
# Smoke-specific invariant: this run just deployed dispatch-echo to all
# replicas, so the placement count must equal replicas. The cluster-wide
# invariants (no skew, no orphans, no unreachable runners) are deferred to
# the assert-placement-consistency script.
placement_count="$(printf '%s' "$fixture_row" | jq '.placements | length')"
if [[ "$placement_count" != "$replicas" ]]; then
  warn "cluster_agents row: $fixture_row"
  fail "GET /cluster/agents reports ${placement_count} placements, expected ${replicas}" 3
fi
assert_placement_consistency "$CLUSTER_AGENTS_FILE"
log "    /cluster/agents: ${replicas} placements, consistent"

log "step 8: sending dispatch smoke request"
smoke_id="k8s-pilot-smoke-$(date +%s)-$$"
dispatch_body="$(jq -n --arg sid "$smoke_id" '{
    routing_key: "pilot.smoke",
    message_type: "k8s-pilot-smoke.v1",
    messages: [],
    task_id: $sid,
    context_id: $sid,
    message_id: $sid
  }')"

dispatch_response="$(curl -sf -X POST \
  "$RUNNER_URL/agents/$FIXTURE_PKG/$FIXTURE_INSTANCE/dispatch" \
  -H 'content-type: application/json' \
  -d "$dispatch_body" || true)"

if [[ -z "$dispatch_response" ]]; then
  fail "dispatch returned no body — runner may have rejected the request" 3
fi
accepted="$(printf '%s' "$dispatch_response" | jq -r '.accepted // false')"
if [[ "$accepted" != "true" ]]; then
  warn "dispatch response: $dispatch_response"
  fail "dispatch did not return accepted=true" 3
fi
detail="$(printf '%s' "$dispatch_response" | jq -r '.detail // ""')"
log "    dispatch accepted: $detail"

if [[ "$KEEP_DEPLOYED" -eq 0 ]]; then
  log "step 9: per-pod undeploy of $FIXTURE_PKG"
  # /cluster/deploy fanned out; the symmetric /cluster/undeploy doesn't
  # exist yet, and a single-runner /undeploy would only remove the agent
  # on whichever pod kube-proxy routed to. Reuse the per-pod
  # port-forwards opened in step 5 and undeploy on each.
  undeploy_body="$(jq -n --arg h "$content_hash" '{hash: $h}')"
  for ((i=0; i<replicas; i++)); do
    pod="${runner_sts}-${i}"
    pod_port=$((PUBLISH_PORT_BASE + i))
    pod_url="http://localhost:${pod_port}"
    if ! curl -sf -X POST "${pod_url}/undeploy" \
          -H "X-Runner-Token: ${RUNNER_TOKEN}" \
          -H 'content-type: application/json' \
          -d "$undeploy_body" >/dev/null; then
      warn "undeploy on pod/${pod} failed — fixture may remain deployed (pass --keep-deployed to silence)"
    fi
  done
fi

log "smoke passed"
