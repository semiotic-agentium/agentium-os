#!/usr/bin/env bash
# Authoritative Kubernetes pilot package-validation entrypoint.
#
# Exercises the supported install surface end-to-end on a local k3d cluster:
# builds the runner image, makes it reachable via the selected image
# strategy, creates the three required objects (surrealdb-credentials,
# runner-token, fnox-config), installs the Helm chart, and then runs the
# documented operator smoke flow (scripts/k8s-pilot-smoke.sh) plus one
# package-wiring verify against SurrealDB.
#
# This script is the in-repo mirror of docs/k8s-pilot-operator-guide.md.
# Its job is to catch regressions that would otherwise only surface when a
# design partner runs `helm upgrade --install` themselves. There is no
# post-install kubectl patching here — if the chart doesn't wire a piece of
# behaviour, this script fails rather than papering over it.
#
# Usage: scripts/verify-k8s-pilot-package.sh [options]
# See --help for flags.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Source lib.sh first — it re-initializes REPO_ROOT/BUILDER_BIN to empty
# defaults for callers that set them later. Same pattern as
# scripts/e2e-k8s/run.sh.
# shellcheck source=e2e-k8s/lib.sh
source "${SCRIPT_DIR}/e2e-k8s/lib.sh"

REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release/baml-agent-builder"

# Defaults. Flags override these.
SKIP_BUILD=false
KEEP_CLUSTER=false
LOCAL_PORT=18080
EXTRA_VALUES=()

usage() {
  cat <<'EOF'
Authoritative Kubernetes pilot package-validation flow.

Usage:
  scripts/verify-k8s-pilot-package.sh [options]

Options:
  --no-build            Skip the `docker build` step (reuse cached image)
  --keep-cluster        Do not delete the k3d cluster on exit
  --image-strategy <s>  local-k3d-import (default) | registry
                        registry is a documented but not-yet-wired extension
                        seam; use it by building and pushing to a cluster-
                        reachable registry, then setting --image-repository
                        and --image-tag.
  --image-repository <r>  Override IMAGE_NAME (default: agentium-runner)
  --image-tag <t>       Override IMAGE_TAG (default: demo)
  --local-port <port>   Local port for the smoke port-forward (default: 18080)
  --values <path>       Extra Helm values file layered on top of the default
                        k3d-values.yaml. Repeatable; later files override
                        earlier ones (standard helm -f semantics).
  -h, --help            Show this message and exit

Environment:
  RUNNER_IMAGE_STRATEGY  Same as --image-strategy (flag wins when both set).

Verifies (in order):
  1. Helm chart installs cleanly with the three required pre-created objects.
  2. Both runner pods reach Ready (implies /readyz 200).
  3. scripts/k8s-pilot-smoke.sh --port-forward succeeds end-to-end
     (publish + deploy + dispatch verification via cargo agent-platform push).
  4. Both runners registered in SurrealDB cluster_runners (count = 2).

Exit codes:
  0  package validation passed
  1  precondition or transport failure
  2  smoke failure (publish/deploy/dispatch)
  3  package-wiring verify failure (cluster_runners count)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)           SKIP_BUILD=true; shift ;;
    --keep-cluster)       KEEP_CLUSTER=true; shift ;;
    --image-strategy)     RUNNER_IMAGE_STRATEGY="$2"; shift 2 ;;
    --image-repository)   IMAGE_NAME="$2"; shift 2 ;;
    --image-tag)          IMAGE_TAG="$2"; shift 2 ;;
    --local-port)         LOCAL_PORT="$2"; shift 2 ;;
    --values)             EXTRA_VALUES+=("$2"); shift 2 ;;
    -h|--help)            usage; exit 0 ;;
    *)                    log_fail "unknown argument: $1"; exit 1 ;;
  esac
done

cleanup() {
  local code=$?
  if (( HAS_FAILURE )) || (( code != 0 )); then
    local dir="./e2e-k8s-logs/verify-$(date +%Y%m%d-%H%M%S)"
    dump_logs "$dir" 2>/dev/null || true
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
  if [[ ! -f "${REPO_ROOT}/${HELM_VALUES_FILE}" ]]; then
    log_fail "Helm values file missing: ${HELM_VALUES_FILE}"
    exit 1
  fi
  log_info "All preflight checks passed."
}

build_image() {
  if [[ "$SKIP_BUILD" == "true" ]]; then
    log_step "Build (skipped — --no-build)"
    return
  fi
  log_step "Building runner image (${IMAGE_NAME}:${IMAGE_TAG})"
  docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" "$REPO_ROOT"
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
  # Run from repo root: the smoke script checks a relative fixture path
  # and invokes `cargo run -p cargo-agent-platform`. Export RUNNER_TOKEN
  # so it skips its own secret lookup and uses the value we just wrote.
  (
    cd "$REPO_ROOT"
    RUNNER_TOKEN="$E2E_TOKEN" \
      bash "${REPO_ROOT}/scripts/k8s-pilot-smoke.sh" \
        --namespace "$NAMESPACE" \
        --service "$RUNNER_API_SERVICE" \
        --local-port "$LOCAL_PORT" \
        --port-forward
  )
}

verify_package_wiring() {
  log_step "Verifying Helm-installed runners registered in cluster_runners"
  local result count
  result="$(surreal_query "SELECT runner_id, endpoint FROM cluster_runners")"
  count="$(echo "$result" | jq '[.[] | .result | .[]] | length')"
  if [[ "$count" != "2" ]]; then
    log_fail "cluster_runners: expected 2 rows, got ${count:-unknown}"
    echo "$result" | jq . >&2 || true
    return 1
  fi
  log_info "cluster_runners: 2 rows (both Helm-installed runners registered)"

  # Confirm the endpoints are the chart-rendered DNS names, not legacy raw
  # manifest names. A silent regression to old names would otherwise pass
  # the count assertion.
  local endpoints
  endpoints="$(echo "$result" | jq -r '[.[] | .result | .[].endpoint] | sort | join(",")')"
  local expected_0 expected_1
  expected_0="$(runner_endpoint 0)"
  expected_1="$(runner_endpoint 1)"
  if ! echo "$endpoints" | grep -qF "$expected_0"; then
    log_fail "cluster_runners missing endpoint ${expected_0} (got: ${endpoints})"
    return 1
  fi
  if ! echo "$endpoints" | grep -qF "$expected_1"; then
    log_fail "cluster_runners missing endpoint ${expected_1} (got: ${endpoints})"
    return 1
  fi
  log_info "cluster_runners endpoints match chart-rendered DNS (${expected_0}, ${expected_1})"
}

main() {
  preflight
  build_image
  create_or_reuse_cluster
  ensure_runner_image_available
  create_pilot_objects
  local helm_values_args=()
  for extra in "${EXTRA_VALUES[@]}"; do
    if [[ ! -f "$extra" ]]; then
      log_fail "--values path not found: ${extra}"
      exit 1
    fi
    helm_values_args+=("-f" "$extra")
    log_info "layering extra Helm values: ${extra}"
  done
  install_pilot_chart "${helm_values_args[@]}"
  resolve_chart_names
  wait_for_runner_readyz
  if ! run_smoke; then
    log_fail "k8s-pilot-smoke.sh failed"
    exit 2
  fi
  if ! verify_package_wiring; then
    exit 3
  fi
  log_step "Package validation PASSED"
  log_info "Authoritative path: Helm install (chart) -> k8s-pilot-smoke -> cluster_runners verify"
  log_info "No post-install kubectl patches were applied."
}

main "$@"
