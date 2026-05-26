#!/usr/bin/env bash
# Inject a latency-spike failure mode via the failure-harness admin API.
#
# Env knobs:
#   NAMESPACE         target namespace (default: agentium-demo)
#   INCIDENT_ID       ledger row id (default: demo-latency-001)
#   DURATION_SECONDS  injection window (default: 300)
#   LATENCY_MS_P95    target p95 latency in ms (default: 1800)
#   ERROR_RATE        injected error rate 0..1 (default: 0.02)
set -euo pipefail

NAMESPACE="${NAMESPACE:-agentium-demo}"
INCIDENT_ID="${INCIDENT_ID:-demo-latency-001}"
DURATION_SECONDS="${DURATION_SECONDS:-300}"
LATENCY_MS_P95="${LATENCY_MS_P95:-1800}"
ERROR_RATE="${ERROR_RATE:-0.02}"

payload=$(cat <<JSON
{
  "mode": "latency_spike",
  "duration_seconds": $DURATION_SECONDS,
  "latency_ms_p95": $LATENCY_MS_P95,
  "error_rate": $ERROR_RATE,
  "incident_id": "$INCIDENT_ID"
}
JSON
)

echo "[inject] incident=$INCIDENT_ID duration=${DURATION_SECONDS}s p95=${LATENCY_MS_P95}ms err=$ERROR_RATE"
kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS -X POST localhost:8080/admin/failure-mode \
    -H 'content-type: application/json' \
    -d "$payload"
echo
echo "[inject] ok"
