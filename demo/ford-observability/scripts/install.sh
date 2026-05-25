#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

NAMESPACE="${NAMESPACE:-agentium-demo}"
RELEASE="${RELEASE:-agentium-observability-demo}"
CHART="${CHART:-./demo/ford-observability/helm}"

helm upgrade --install "$RELEASE" "$CHART" \
  --namespace "$NAMESPACE" \
  --create-namespace \
  "$@"
