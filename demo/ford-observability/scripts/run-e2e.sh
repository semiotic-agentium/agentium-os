#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

./demo/ford-observability/scripts/install.sh
./demo/ford-observability/scripts/inject-latency.sh

echo "Waiting for alert/investigation path (placeholder; full smoke checks land later)."
sleep "${DEMO_E2E_WAIT_SECONDS:-30}"

NAMESPACE="${NAMESPACE:-agentium-demo}"
kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS localhost:8080/admin/ledger || true
