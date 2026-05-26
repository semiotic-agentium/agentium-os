#!/usr/bin/env bash
# Install (or upgrade) the ford-observability demo via Helm.
#
# Env knobs:
#   NAMESPACE        target namespace (default: agentium-demo)
#   RELEASE          helm release name (default: agentium-observability-demo)
#   CHART            path to chart (default: <demo>/helm)
#   VALUES_FILE      optional extra -f values file
#   WAIT_ROLLOUTS    1 to block on rollout status, 0 to skip (default: 1)
#   ROLLOUT_TIMEOUT  kubectl rollout timeout (default: 5m)
#
# Extra args are forwarded to `helm upgrade --install`, e.g.:
#   ./install.sh --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

NAMESPACE="${NAMESPACE:-agentium-demo}"
RELEASE="${RELEASE:-agentium-observability-demo}"
CHART="${CHART:-$DEMO_DIR/helm}"
WAIT_ROLLOUTS="${WAIT_ROLLOUTS:-1}"
ROLLOUT_TIMEOUT="${ROLLOUT_TIMEOUT:-5m}"

helm_args=(upgrade --install "$RELEASE" "$CHART"
  --namespace "$NAMESPACE"
  --create-namespace)

if [[ -n "${VALUES_FILE:-}" ]]; then
  helm_args+=(-f "$VALUES_FILE")
fi

echo "[install] helm ${helm_args[*]} $*"
helm "${helm_args[@]}" "$@"

if [[ "$WAIT_ROLLOUTS" != "1" ]]; then
  exit 0
fi

workloads=(
  checkout-api
  payments-api
  failure-harness
  grafana
  prometheus
  loki
  alloy
  k6-load-generator
  agentium-runner
)

echo "[install] waiting for rollouts (timeout=$ROLLOUT_TIMEOUT)"
for w in "${workloads[@]}"; do
  if kubectl -n "$NAMESPACE" get deploy "$w" >/dev/null 2>&1; then
    kubectl -n "$NAMESPACE" rollout status "deploy/$w" --timeout="$ROLLOUT_TIMEOUT"
  elif kubectl -n "$NAMESPACE" get statefulset "$w" >/dev/null 2>&1; then
    kubectl -n "$NAMESPACE" rollout status "statefulset/$w" --timeout="$ROLLOUT_TIMEOUT"
  else
    echo "[install] skip $w (not found in $NAMESPACE)"
  fi
done

echo "[install] done. namespace=$NAMESPACE release=$RELEASE"
