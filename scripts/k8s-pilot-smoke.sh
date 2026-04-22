#!/usr/bin/env bash
# Kubernetes pilot first-run smoke flow. See `--help` for usage.
# Packages steps 6–8 of docs/k8s-pilot-operator-guide.md.

set -euo pipefail

usage() {
  cat <<'EOF'
Kubernetes pilot first-run smoke flow.

Usage:
  bash scripts/k8s-pilot-smoke.sh [options]

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
                       Pick a different port if localhost:18080 is already
                       in use (e.g. by a local dev runner).
  --keep-deployed      Do not undeploy the fixture at the end
  -h, --help           Show this message and exit

Environment:
  RUNNER_TOKEN         If set, used directly. Otherwise the script reads it
                       from the named Kubernetes secret.

Exit codes:
  0  smoke passed
  1  precondition or transport failure
  2  publish/deploy failure
  3  dispatch verification failure
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
KEEP_DEPLOYED=0
PF_PID=""
PF_LOG=""

FIXTURE_PATH="tests/fixtures/agents/dispatch-echo"
FIXTURE_PKG="dispatch-echo"
FIXTURE_INSTANCE="default"

log()     { printf '==> %s\n' "$*"; }
warn()    { printf '  ! %s\n' "$*" >&2; }
fail()    { printf '  x %s\n' "$1" >&2; exit "${2:-1}"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)      NAMESPACE="$2"; shift 2 ;;
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

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1 (install it and re-run)" 1
}

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
  if [[ -n "$PF_LOG" && -f "$PF_LOG" ]]; then
    rm -f "$PF_LOG"
  fi
  exit "$code"
}
trap cleanup EXIT INT TERM

log "step 1: resolving runner token"
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

healthz_ok=0
if [[ "$DO_PORT_FORWARD" -eq 1 ]]; then
  log "step 2: opening port-forward to svc/$API_SERVICE on localhost:$LOCAL_PORT"
  # Refuse to run if something already binds the local port — otherwise the
  # /healthz poll below could succeed against a local dev runner and the rest
  # of the smoke would push/deploy to the wrong process.
  if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:$LOCAL_PORT/healthz" 2>/dev/null; then
    fail "localhost:$LOCAL_PORT already responds to /healthz — another process is bound here. Stop it or re-run with --local-port <port>." 1
  fi
  PF_LOG="$(mktemp)"
  kubectl -n "$NAMESPACE" port-forward "svc/$API_SERVICE" "$LOCAL_PORT:$API_PORT" \
    >"$PF_LOG" 2>&1 &
  PF_PID=$!
  RUNNER_URL="http://localhost:$LOCAL_PORT"
  for _ in $(seq 1 60); do
    if ! kill -0 "$PF_PID" 2>/dev/null; then
      fail "kubectl port-forward exited before becoming ready: $(cat "$PF_LOG")" 1
    fi
    if curl -sf -o /dev/null --connect-timeout 1 "$RUNNER_URL/healthz" 2>/dev/null; then
      healthz_ok=1
      break
    fi
    sleep 0.5
  done
  if [[ "$healthz_ok" -ne 1 ]]; then
    fail "port-forward did not become ready within 30s: $(cat "$PF_LOG")" 1
  fi
fi

log "step 3: probing runner at $RUNNER_URL"
if [[ "$healthz_ok" -ne 1 ]] && ! curl -sf "$RUNNER_URL/healthz" >/dev/null; then
  fail "runner not reachable at $RUNNER_URL/healthz. Install the chart and/or open a port-forward first." 1
fi
if ! curl -sf "$RUNNER_URL/readyz" >/dev/null; then
  fail "runner /readyz is failing. Check pod status and SurrealDB connectivity." 1
fi

log "step 4: authenticated publish+deploy via cargo agent-platform push"
if ! cargo run -q -p cargo-agent-platform -- push \
      --agents "$FIXTURE_PATH" \
      --url "$RUNNER_URL" \
      --repository-url "$RUNNER_URL/repository" \
      --runner-token "$RUNNER_TOKEN"; then
  fail "cargo agent-platform push failed — see output above" 2
fi

log "step 5: verifying $FIXTURE_PKG is visible on /agents"
agents_json="$(curl -sf "$RUNNER_URL/agents" || true)"
if [[ -z "$agents_json" ]]; then
  fail "GET /agents returned empty body" 3
fi
content_hash="$(printf '%s' "$agents_json" \
  | jq -r --arg pkg "$FIXTURE_PKG" \
      '.[] | select(.agent_package == $pkg) | .agent_card.content_hash // empty' \
  | head -1)"
if [[ -z "$content_hash" ]]; then
  fail "$FIXTURE_PKG not listed on /agents after deploy" 3
fi
log "    deployed content_hash=$content_hash"

log "step 6: sending dispatch smoke request"
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
  log "step 7: undeploying $FIXTURE_PKG"
  undeploy_body="$(jq -n --arg h "$content_hash" '{hash: $h}')"
  if ! curl -sf -X POST "$RUNNER_URL/undeploy" \
        -H "X-Runner-Token: $RUNNER_TOKEN" \
        -H 'content-type: application/json' \
        -d "$undeploy_body" >/dev/null; then
    warn "undeploy failed — fixture remains deployed (pass --keep-deployed to silence)"
  fi
fi

log "smoke passed"
