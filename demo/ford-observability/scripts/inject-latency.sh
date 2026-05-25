#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${NAMESPACE:-agentium-demo}"
INCIDENT_ID="${INCIDENT_ID:-demo-latency-001}"
DURATION_SECONDS="${DURATION_SECONDS:-300}"
LATENCY_MS_P95="${LATENCY_MS_P95:-1800}"

kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS -X POST localhost:8080/admin/failure-mode \
    -H 'content-type: application/json' \
    -d "{\"mode\":\"latency_spike\",\"duration_seconds\":$DURATION_SECONDS,\"latency_ms_p95\":$LATENCY_MS_P95,\"incident_id\":\"$INCIDENT_ID\"}"
