#!/usr/bin/env bash
# Installs/upgrades Argo CD via Helm for local k3d.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
  echo "argocd-install: $*" >&2
  exit 1
}

require_cmd() {
  local cmd
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || die "required command not found: ${cmd}"
  done
}

require_cmd helm kubectl

ARGOCD_VALUES="${ROOT}/deploy/values/local/argocd.yaml"
[[ -f "${ARGOCD_VALUES}" ]] || die "missing ${ARGOCD_VALUES}"

helm repo add argo https://argoproj.github.io/argo-helm 2>/dev/null || true

if [[ "${SKIP_HELM_REPO_UPDATE:-0}" != "1" ]]; then
  helm repo update argo || die "helm repo update argo failed (retry with SKIP_HELM_REPO_UPDATE=1 if cached)"
fi

HELM_WAIT_ARGS=()
if [[ "${AGENTIUM_ARGO_HELM_WAIT:-1}" == "1" ]]; then
  HELM_WAIT_ARGS=(--wait "--timeout=${AGENTIUM_ARGO_HELM_TIMEOUT:-20m}")
fi

helm upgrade --install argocd argo/argo-cd \
  --namespace argocd \
  --create-namespace \
  --values "${ARGOCD_VALUES}" \
  "${HELM_WAIT_ARGS[@]}"

echo "argocd-install: Argo CD ready in namespace argocd"
