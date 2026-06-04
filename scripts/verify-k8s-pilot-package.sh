#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Authoritative Kubernetes pilot package-validation entrypoint.
#
# Exercises the supported install surface end-to-end on a local k3d cluster:
# builds the runner image, pushes to the local registry, creates required
# objects, installs via Argo CD sync, and runs k8s-pilot-smoke.sh.
#
# Usage: scripts/verify-k8s-pilot-package.sh [options]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=e2e-k8s/lib.sh
source "${SCRIPT_DIR}/e2e-k8s/lib.sh"

REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release/baml-agent-builder"

SKIP_BUILD=false
KEEP_CLUSTER=false
SMOKE_KEEP_DEPLOYED=false
LOCAL_PORT=18080
EXTRA_VALUES=()

usage() {
  cat <<'EOF'
Authoritative Kubernetes pilot package-validation flow (Argo CD + local registry).

Usage:
  scripts/verify-k8s-pilot-package.sh [options]

Options:
  --no-build            Skip the `docker build` step (reuse cached image)
  --keep-cluster        Do not delete the k3d cluster on exit
  --smoke-keep-deployed Keep dispatch-echo deployed after verify completes
  --image-tag <t>       Override image tag (default: nonce from .last-image-tag)
  --local-port <port>   Local port for smoke port-forward (default: 18080)
  --values <path>       Scenario values overlay (single file → generated/scenario.yaml)
  -h, --help            Show this message and exit

Environment:
  AGENTIUM_IMAGE_TAG    Same as --image-tag

Verifies:
  1. Argo CD installs agentium-os with registry-backed runner image
  2. Both runner pods reach Ready (/readyz 200)
  3. Runner pod imageIDs match pushed registry digest
  4. k8s-pilot-smoke.sh succeeds
  5. cluster_runners has 2 rows
  6. No unexpected WARN logs
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)            SKIP_BUILD=true; shift ;;
    --keep-cluster)        KEEP_CLUSTER=true; shift ;;
    --smoke-keep-deployed) SMOKE_KEEP_DEPLOYED=true; shift ;;
    --image-tag)           AGENTIUM_IMAGE_TAG="$2"; shift 2 ;;
    --local-port)          LOCAL_PORT="$2"; shift 2 ;;
    --values)              EXTRA_VALUES+=("$2"); shift 2 ;;
    -h|--help)             usage; exit 0 ;;
    *)                     log_fail "unknown argument: $1"; exit 1 ;;
  esac
done

LOG_DIR="${REPO_ROOT}/e2e-k8s-logs/verify-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$LOG_DIR"

cleanup() {
  local code=$?
  if (( HAS_FAILURE )) || (( code != 0 )); then
    dump_logs "$LOG_DIR" 2>/dev/null || true
  fi
  if [[ "$KEEP_CLUSTER" == "false" ]]; then
    k3d cluster delete "$CLUSTER_NAME" 2>/dev/null || true
  else
    log_info "Cluster '$CLUSTER_NAME' kept (--keep-cluster)."
  fi
  exit "$code"
}
trap cleanup EXIT INT TERM

preflight() {
  log_step "Preflight checks"
  local missing=()
  for cmd in docker k3d kubectl helm jq curl cargo; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    log_fail "Missing required tools: ${missing[*]}"
    exit 1
  fi
  preflight_container_runtime
  log_info "All preflight checks passed."
}

build_image() {
  if [[ "$SKIP_BUILD" == "true" ]]; then
    log_step "Build (skipped — --no-build)"
    ensure_image_tag_or_nonce
    return
  fi
  ensure_image_tag_or_nonce
  log_step "Building runner image (${IMAGE_NAME}:${IMAGE_TAG})"
  docker build \
    --build-arg "VERSION=$(bash "${REPO_ROOT}/scripts/release/workspace-version.sh")" \
    -t "${IMAGE_NAME}:${IMAGE_TAG}" "$REPO_ROOT"
}

create_or_reuse_cluster() {
  log_step "Creating k3d cluster '${CLUSTER_NAME}'"
  if k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"${CLUSTER_NAME}\""; then
    log_info "Cluster '${CLUSTER_NAME}' already exists — deleting for a clean run."
    k3d cluster delete "$CLUSTER_NAME"
  fi
  k3d cluster create --config "${REPO_ROOT}/deploy/k3d/cluster.yaml"
}

run_smoke() {
  log_step "Running scripts/k8s-pilot-smoke.sh (port-forward mode)"
  local smoke_args=(
    --namespace "$NAMESPACE"
    --service "$RUNNER_API_SERVICE"
    --local-port "$LOCAL_PORT"
    --port-forward
  )
  if [[ "$SMOKE_KEEP_DEPLOYED" == "true" ]]; then
    smoke_args+=(--keep-deployed)
  fi
  (
    cd "$REPO_ROOT"
    RUNNER_TOKEN="$E2E_TOKEN" \
    K8S_PILOT_PF_LOG_DIR="$LOG_DIR" \
      bash "${REPO_ROOT}/scripts/k8s-pilot-smoke.sh" "${smoke_args[@]}"
  )
}

verify_package_wiring() {
  log_step "Verifying Helm-installed runners registered in cluster_runners"
  local result count
  if ! result="$(surreal_query "SELECT runner_id, endpoint FROM cluster_runners")"; then
    log_fail "cluster_runners query failed"
    return 1
  fi
  count="$(echo "$result" | jq '[.[] | .result | .[]] | length')"
  if [[ "$count" != "2" ]]; then
    log_fail "cluster_runners: expected 2 rows, got ${count:-unknown}"
    return 1
  fi
  log_info "cluster_runners: 2 rows"
}

verify_no_warn_logs() {
  log_step "Scanning runner + SurrealDB logs for unexpected WARN lines"
  (
    cd "$REPO_ROOT"
    bash "${REPO_ROOT}/scripts/k8s-pilot-assert-no-warn-logs.sh" \
      --namespace "$NAMESPACE" \
      --release "$RELEASE_NAME"
  )
}

main() {
  preflight
  build_image
  create_or_reuse_cluster
  if ((${#EXTRA_VALUES[@]} > 0)); then
    if ((${#EXTRA_VALUES[@]} > 1)); then
      log_warn "multiple --values files; using last: ${EXTRA_VALUES[-1]}"
    fi
    export AGENTIUM_SCENARIO_VALUES="${EXTRA_VALUES[-1]}"
  fi
  bringup_pilot_stack
  if ! verify_pod_image_digests; then
    exit 4
  fi
  if ! run_smoke; then
    exit 2
  fi
  if ! verify_package_wiring; then
    exit 3
  fi
  if ! verify_no_warn_logs; then
    exit 5
  fi
  log_step "Package validation PASSED"
}

main "$@"
