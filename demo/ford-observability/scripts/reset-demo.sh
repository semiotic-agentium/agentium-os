#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${NAMESPACE:-agentium-demo}"

kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS -X POST localhost:8080/admin/reset-active || true
kubectl -n "$NAMESPACE" exec deploy/failure-harness -- \
  curl -fsS -X POST localhost:8080/admin/reset-ledger || true
