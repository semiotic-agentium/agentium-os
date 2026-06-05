#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

require_cmd() {
  local cmd
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || {
      echo "argocd-bootstrap-apps: required command not found: ${cmd}" >&2
      exit 1
    }
  done
}

require_cmd kubectl

bash "${ROOT}/scripts/e2e-k8s/render-argocd-apps.sh"

kubectl apply -f "${ROOT}/deploy/argocd/.rendered/project.yaml"
kubectl apply -f "${ROOT}/deploy/argocd/.rendered/root-application.yaml"

echo "argocd-bootstrap-apps: applied AppProject + agentium-os Application"
