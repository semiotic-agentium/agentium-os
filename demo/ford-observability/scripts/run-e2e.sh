#!/usr/bin/env bash
# Smoke end-to-end: install -> inject -> wait for coordinator context ->
# dump ledger + conversation history + provenance to OUT_DIR.
#
# Ledger-assertion (citation matching) is nice-to-have and not run here.
#
# Env knobs:
#   NAMESPACE        target namespace (default: agentium-demo)
#   RUNNER_SVC       runner service name (default: agentium-runner)
#   RUNNER_PORT      runner service port (default: 18080)
#   INCIDENT_ID      ledger row id (default: demo-e2e-<epoch>)
#   AGENT_PACKAGE    coordinator package filter (default: observability-coordinator)
#   WAIT_SECONDS     max wait for coordinator context (default: 240)
#   POLL_SECONDS     poll interval (default: 5)
#   OUT_DIR          artifact dir (default: <demo>/.e2e-out/<incident_id>)
#   SKIP_INSTALL     1 to skip helm install step (default: 0)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

NAMESPACE="${NAMESPACE:-agentium-demo}"
RUNNER_SVC="${RUNNER_SVC:-agentium-runner}"
RUNNER_PORT="${RUNNER_PORT:-18080}"
INCIDENT_ID="${INCIDENT_ID:-demo-e2e-$(date +%s)}"
AGENT_PACKAGE="${AGENT_PACKAGE:-observability-coordinator}"
WAIT_SECONDS="${WAIT_SECONDS:-240}"
POLL_SECONDS="${POLL_SECONDS:-5}"
OUT_DIR="${OUT_DIR:-$DEMO_DIR/.e2e-out/$INCIDENT_ID}"
SKIP_INSTALL="${SKIP_INSTALL:-0}"

mkdir -p "$OUT_DIR"
echo "[e2e] artifacts -> $OUT_DIR"

if [[ "$SKIP_INSTALL" != "1" ]]; then
  NAMESPACE="$NAMESPACE" "$SCRIPT_DIR/install.sh"
fi

# Port-forward the runner so we can hit /contexts and provenance APIs from host.
PF_LOG="$OUT_DIR/port-forward.log"
LOCAL_PORT="${LOCAL_PORT:-18080}"
echo "[e2e] port-forwarding svc/$RUNNER_SVC $LOCAL_PORT -> $RUNNER_PORT"
kubectl -n "$NAMESPACE" port-forward "svc/$RUNNER_SVC" "$LOCAL_PORT:$RUNNER_PORT" >"$PF_LOG" 2>&1 &
PF_PID=$!
cleanup() {
  if kill -0 "$PF_PID" 2>/dev/null; then
    kill "$PF_PID" 2>/dev/null || true
    wait "$PF_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

RUNNER_URL="http://127.0.0.1:$LOCAL_PORT"

# Wait for readyz.
echo "[e2e] waiting for runner readyz at $RUNNER_URL/readyz"
for _ in $(seq 1 60); do
  if curl -fsS "$RUNNER_URL/readyz" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
curl -fsS "$RUNNER_URL/readyz" >/dev/null || {
  echo "[e2e] runner never became ready" >&2
  exit 1
}

# Snapshot baseline /contexts so we can distinguish the new investigation.
baseline_ctx="$OUT_DIR/contexts-baseline.json"
curl -fsS "$RUNNER_URL/contexts?agentPackage=$AGENT_PACKAGE&limit=50" >"$baseline_ctx" || echo '{}' >"$baseline_ctx"
baseline_ids=$(jq -r '.items[]?.contextId // .items[]?.context_id // empty' "$baseline_ctx" 2>/dev/null | sort -u || true)

# Inject failure.
INCIDENT_ID="$INCIDENT_ID" NAMESPACE="$NAMESPACE" "$SCRIPT_DIR/inject-latency.sh"

# Poll for a new coordinator context to appear.
echo "[e2e] polling for new $AGENT_PACKAGE context (timeout=${WAIT_SECONDS}s)"
context_id=""
deadline=$(( $(date +%s) + WAIT_SECONDS ))
while [[ $(date +%s) -lt $deadline ]]; do
  curl -fsS "$RUNNER_URL/contexts?agentPackage=$AGENT_PACKAGE&limit=50" >"$OUT_DIR/contexts-latest.json" || true
  current_ids=$(jq -r '.items[]?.contextId // .items[]?.context_id // empty' "$OUT_DIR/contexts-latest.json" 2>/dev/null | sort -u || true)
  new_ids=$(comm -23 <(echo "$current_ids") <(echo "$baseline_ids") || true)
  if [[ -n "$new_ids" ]]; then
    context_id=$(echo "$new_ids" | head -n1)
    break
  fi
  sleep "$POLL_SECONDS"
done

if [[ -z "$context_id" ]]; then
  echo "[e2e] no new coordinator context appeared within ${WAIT_SECONDS}s" >&2
  echo "[e2e] dumping ledger for debug:" >&2
  kubectl -n "$NAMESPACE" exec statefulset/failure-harness -- \
    curl -fsS localhost:8080/admin/ledger >"$OUT_DIR/ledger.json" || true
  cat "$OUT_DIR/ledger.json" >&2 || true
  exit 1
fi

echo "[e2e] context_id=$context_id"
echo "$context_id" >"$OUT_DIR/context_id.txt"

# Pull artifacts.
echo "[e2e] fetching conversation-history + provenance"
curl -fsS "$RUNNER_URL/contexts/$context_id/conversation-history" >"$OUT_DIR/conversation-history.json" || true
curl -fsS "$RUNNER_URL/provenance/llm-calls?context_id=$context_id" >"$OUT_DIR/llm-calls.json" || true
curl -fsS "$RUNNER_URL/provenance/tool-calls?context_id=$context_id" >"$OUT_DIR/tool-calls.json" || true

kubectl -n "$NAMESPACE" exec statefulset/failure-harness -- \
  curl -fsS "localhost:8080/admin/ledger/$INCIDENT_ID" >"$OUT_DIR/ledger.json" || \
  kubectl -n "$NAMESPACE" exec statefulset/failure-harness -- \
    curl -fsS localhost:8080/admin/ledger >"$OUT_DIR/ledger.json" || true

cat <<EOF

[e2e] done.
  incident_id   = $INCIDENT_ID
  context_id    = $context_id
  artifacts     = $OUT_DIR
  dashboard URL = http://127.0.0.1:$LOCAL_PORT/?view=dashboard&contextId=$context_id
EOF
