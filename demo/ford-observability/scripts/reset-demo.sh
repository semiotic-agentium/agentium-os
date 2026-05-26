#!/usr/bin/env bash
# Stop active failure injection and reset the ground-truth ledger.
#
# Env knobs:
#   NAMESPACE   target namespace (default: agentium-demo)
#   KEEP_LEDGER 1 to skip ledger reset (default: 0)
set -euo pipefail

NAMESPACE="${NAMESPACE:-agentium-demo}"
KEEP_LEDGER="${KEEP_LEDGER:-0}"

echo "[reset] stopping any active failure mode"
kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS -X POST localhost:8080/admin/reset-active || true
echo

if [[ "$KEEP_LEDGER" != "1" ]]; then
  echo "[reset] clearing ledger"
  kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
    curl -fsS -X POST localhost:8080/admin/reset-ledger || true
  echo
fi

echo "[reset] done"
