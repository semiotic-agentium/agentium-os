#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Sync the agentium-os Argo CD Application (job mode with k3d registry, or kubectl refresh).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
  echo "sync-argocd: $*" >&2
  exit 1
}

require_cmd() {
  local cmd
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || die "required command not found: ${cmd}"
  done
}

require_cmd kubectl

NS_ARGO="${ARGOCD_APP_NAMESPACE:-argocd}"
REGISTRY_CONTAINER_HOST="${REGISTRY_CONTAINER_HOST:-k3d-agentium-registry}"
REGISTRY_HOST_PORT="${REGISTRY_HOST_PORT:-5400}"
MODE="${AGENTIUM_ARGO_SYNC_MODE:-auto}"

_local_registry_pushable() {
  command -v docker >/dev/null 2>&1 || return 1
  docker ps --format '{{.Names}}' 2>/dev/null | grep -q "${REGISTRY_CONTAINER_HOST}" || return 1
  curl -sf -o /dev/null --connect-timeout 2 "http://localhost:${REGISTRY_HOST_PORT}/v2/" 2>/dev/null
}

if [[ "${MODE}" == "auto" ]]; then
  if _local_registry_pushable; then
    MODE="job"
  else
    MODE="kubectl"
    echo "sync-argocd: auto → kubectl (no local registry — run cluster bringup first)" >&2
  fi
fi

if [[ ! -f "${ROOT}/deploy/values/generated/images.yaml" ]]; then
  bash "${ROOT}/scripts/e2e-k8s/render-values.sh"
fi

_SYNC_PATCH='{"spec":{"operation":{"initiatedBy":{"username":"agentium-sync-argocd"},"sync":{"prune":true,"syncStrategy":{"hook":{}}}}}}'

_sync_kubectl_only() {
  echo "sync-argocd: mode=kubectl"
  kubectl annotate application agentium-os -n "${NS_ARGO}" argocd.argoproj.io/refresh=hard --overwrite
  kubectl patch application agentium-os -n "${NS_ARGO}" --type merge -p "${_SYNC_PATCH}"
}

_sync_via_job() {
  require_cmd docker
  local SYNC_TAG="${AGENTIUM_ARGO_SYNC_IMAGE_TAG:-local-argocd-sync}"
  local HOST_REGISTRY="localhost:${REGISTRY_HOST_PORT}"
  local IN_CLUSTER_IMAGE="${REGISTRY_CONTAINER_HOST}:5000/agentium-argocd-sync:${SYNC_TAG}"
  local DOCKERFILE="${ROOT}/docker/agentium-argocd-sync.Dockerfile"

  echo "sync-argocd: mode=job — building ${IN_CLUSTER_IMAGE}"
  docker build -f "${DOCKERFILE}" -t "${HOST_REGISTRY}/agentium-argocd-sync:${SYNC_TAG}" "${ROOT}"

  local -a push_args=()
  if docker push --help 2>&1 | grep -q -- "--tls-verify"; then
    push_args+=("--tls-verify=false")
  fi
  docker push "${push_args[@]}" "${HOST_REGISTRY}/agentium-argocd-sync:${SYNC_TAG}"

  local ARGOCD_SERVER="${ARGOCD_SERVER:-argocd-server.argocd.svc.cluster.local:443}"
  local job_tmp
  job_tmp="$(mktemp "${TMPDIR:-/tmp}/agentium-argocd-sync-job.XXXXXX")"
  cat >"${job_tmp}" <<'EOF'
apiVersion: batch/v1
kind: Job
metadata:
  name: agentium-argocd-local-sync
  namespace: argocd
spec:
  ttlSecondsAfterFinished: 600
  backoffLimit: 0
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: sync
          image: __IN_CLUSTER_IMAGE__
          imagePullPolicy: Always
          env:
            - name: ARGOCD_SERVER
              value: "__ARGOCD_SERVER__"
            - name: ARGOCD_ADMIN_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: argocd-initial-admin-secret
                  key: password
          command: ["/bin/bash", "-c"]
          args:
            - |
              set -euo pipefail
              export HOME=/tmp
              _argo_login_retry() {
                local mode="$1" server="$2" attempt
                for ((attempt=1; attempt<=10; attempt++)); do
                  if [[ "${mode}" == "tls" ]]; then
                    if argocd login "${server}" --username admin --password "${ARGOCD_ADMIN_PASSWORD}" --grpc-web --insecure --skip-test-tls; then
                      _argo_common=(--server "${server}" --grpc-web --insecure)
                      return 0
                    fi
                  else
                    if argocd login "${server}" --username admin --password "${ARGOCD_ADMIN_PASSWORD}" --grpc-web --plaintext; then
                      _argo_common=(--server "${server}" --grpc-web --plaintext)
                      return 0
                    fi
                  fi
                  sleep 2
                done
                return 1
              }
              if ! _argo_login_retry tls "${ARGOCD_SERVER}"; then
                _ARGOCD_SERVER_PLAINTEXT="${ARGOCD_SERVER_PLAINTEXT:-argocd-server.argocd.svc.cluster.local:80}"
                _argo_login_retry plaintext "${_ARGOCD_SERVER_PLAINTEXT}"
              fi
              argocd app sync agentium-os \
                --local /workspace/deploy/helm/agentium-os \
                --local-repo-root /workspace \
                --app-namespace argocd \
                --prune "${_argo_common[@]}"
EOF
  perl -0pi -e 's|__IN_CLUSTER_IMAGE__|'"${IN_CLUSTER_IMAGE}"'|g; s|__ARGOCD_SERVER__|'"${ARGOCD_SERVER}"'|g' "${job_tmp}"

  kubectl delete job agentium-argocd-local-sync -n argocd --ignore-not-found >/dev/null 2>&1 || true
  kubectl apply -f "${job_tmp}"
  rm -f "${job_tmp}"

  local _timeout_sec="${AGENTIUM_ARGO_SYNC_JOB_TIMEOUT_SECONDS:-600}"
  local _deadline _now succeeded failed
  _deadline=$(($(date +%s) + _timeout_sec))
  echo "sync-argocd: waiting for Job/agentium-argocd-local-sync (timeout ${_timeout_sec}s)..."
  while true; do
    _now=$(date +%s)
    if [[ "${_now}" -ge "${_deadline}" ]]; then
      kubectl logs -n argocd job/agentium-argocd-local-sync --all-containers=true 2>&1 || true
      die "Job agentium-argocd-local-sync timed out"
    fi
    succeeded="$(kubectl get job agentium-argocd-local-sync -n argocd -o jsonpath='{.status.succeeded}' 2>/dev/null || echo "")"
    failed="$(kubectl get job agentium-argocd-local-sync -n argocd -o jsonpath='{.status.failed}' 2>/dev/null || echo "")"
    succeeded="${succeeded:-0}"
    failed="${failed:-0}"
    if [[ "${succeeded}" == "1" ]]; then
      kubectl logs -n argocd job/agentium-argocd-local-sync --all-containers=true 2>&1 || true
      echo "sync-argocd: job succeeded"
      return 0
    fi
    if [[ -n "${failed}" ]] && [[ "${failed}" != "0" ]]; then
      kubectl describe job agentium-argocd-local-sync -n argocd >&2 || true
      kubectl logs -n argocd job/agentium-argocd-local-sync --all-containers=true 2>&1 || true
      die "Job agentium-argocd-local-sync failed"
    fi
    sleep 2
  done
}

# Drop a stale direct-helm release so Argo owns the releaseName.
if command -v helm >/dev/null 2>&1; then
  if helm status agentium -n agentium >/dev/null 2>&1; then
    if ! kubectl get application agentium-os -n argocd >/dev/null 2>&1; then
      echo "sync-argocd: uninstalling legacy direct Helm release agentium" >&2
      helm uninstall agentium -n agentium --wait --timeout 180s || true
    fi
  fi
fi

case "${MODE}" in
  kubectl) _sync_kubectl_only ;;
  job) _sync_via_job ;;
  *) die "unknown AGENTIUM_ARGO_SYNC_MODE=${MODE} (use job, kubectl, or auto)" ;;
esac

echo "sync-argocd: ok"
