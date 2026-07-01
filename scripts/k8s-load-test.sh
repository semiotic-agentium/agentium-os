#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Kubernetes pilot load-test orchestrator.
#
# Reuses scripts/verify-k8s-pilot-package.sh for supported-install-path
# bringup, then runs the three canonical #226 scenarios (local_a2a,
# forwarded_a2a, split_dual_runner) against the Helm-installed pilot.
#
# See docs/runbooks/k8s-pilot-load-testing.md for operator-facing usage.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Reuses publish_fixture / deploy_hash / resolve_chart_names from lib.sh.
# lib.sh generates a random E2E_TOKEN on source and resets REPO_ROOT=""; we
# override both AFTER sourcing (token is read from the cluster secret in
# resync_state_with_cluster — otherwise it won't match the installed secret).
# shellcheck source=e2e-k8s/lib.sh
source "${SCRIPT_DIR}/e2e-k8s/lib.sh"

REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release/agentium"

SCENARIOS="local_a2a,forwarded_a2a,split_dual_runner"
CONCURRENCY="1,8,32"
WARMUP_SECONDS=30
MEASURED_SECONDS=120
PAYLOAD="dispatch-echo load probe"
EXPECT_SUBSTRING="dispatch-echo does not handle A2A messages"
PACKAGE="dispatch-echo"
INSTANCE="default"
SKIP_BRINGUP=false
SKIP_OBSERVABILITY=false
SKIP_BUILDER=false
ARTIFACTS_DIR=""
VERIFY_EXTRA_ARGS=()

LOAD_PF_PIDS=()
LOAD_PF_LOGS=()
OBSERVABILITY_COMPOSE_FILE="${REPO_ROOT}/observability/docker-compose.yml"
OTLP_HOST_PORT="4317"
PROMETHEUS_URL="http://localhost:9090"
OTLP_ENDPOINT="http://host.k3d.internal:4317"
VALUES_OVERLAY="${REPO_ROOT}/deploy/helm/agentium-os/examples/k3d-load-test-values.yaml"
SCENARIO_RESULTS_JSON=()

usage() {
  cat <<'EOF'
Usage:
  scripts/k8s-load-test.sh [options]

Options:
  --scenarios <csv>         Default: local_a2a,forwarded_a2a,split_dual_runner
  --concurrency <csv>       Default: 1,8,32
  --warmup-seconds <n>      Default: 30
  --measured-seconds <n>    Default: 120
  --payload <text>          Fixed request payload. Default: "dispatch-echo load probe"
  --package <name>          Default: dispatch-echo
  --instance <id>           Default: default
  --skip-bringup            Assume cluster is already up (and port-bound).
                            Skips verify-k8s-pilot-package.sh.
  --skip-observability      Do not start observability/docker-compose.yml.
  --skip-builder-build      Do not cargo-build baml-agent-builder even if missing.
  --artifacts-dir <path>    Default: artifacts/load-test/<timestamp>
  --verify-arg <arg>        Repeatable: passthrough arg to verify-k8s-pilot-package.sh.
                            Example: --verify-arg --no-build
  -h, --help

What this does:
  1. Preflight: build baml-agent-builder if missing (publish_fixture needs it).
  2. Start local observability stack (unless --skip-observability).
  3. Bring up the supported Helm package via
     scripts/verify-k8s-pilot-package.sh --keep-cluster --values <overlay>
     (unless --skip-bringup).
  4. Resync wrapper shell with cluster state (read runner-token from K8s;
     call resolve_chart_names locally).
  5. In-pod OTLP TCP probe (host.k3d.internal:4317) via `kubectl exec node`.
  6. Safe port-forwards to runner-0 / runner-1 with pre-bind check and
     liveness polling (matches scripts/k8s-pilot-smoke.sh:118-141 pattern).
  7. Per scenario: publish/deploy, run scripts/load-test/run.mjs, query
     Prometheus deltas, record results.
  8. Emit summary.json + summary.md to --artifacts-dir.

This orchestrator keeps the k3d cluster and observability stack running
on success. `docker compose -f observability/docker-compose.yml down` and
`k3d cluster delete agentium` clean up.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --scenarios)           SCENARIOS="$2"; shift 2 ;;
    --concurrency)         CONCURRENCY="$2"; shift 2 ;;
    --warmup-seconds)      WARMUP_SECONDS="$2"; shift 2 ;;
    --measured-seconds)    MEASURED_SECONDS="$2"; shift 2 ;;
    --payload)             PAYLOAD="$2"; shift 2 ;;
    --package)             PACKAGE="$2"; shift 2 ;;
    --instance)            INSTANCE="$2"; shift 2 ;;
    --skip-bringup)        SKIP_BRINGUP=true; shift ;;
    --skip-observability)  SKIP_OBSERVABILITY=true; shift ;;
    --skip-builder-build)  SKIP_BUILDER=true; shift ;;
    --artifacts-dir)       ARTIFACTS_DIR="$2"; shift 2 ;;
    --verify-arg)          VERIFY_EXTRA_ARGS+=("$2"); shift 2 ;;
    -h|--help)             usage; exit 0 ;;
    *)                     log_fail "unknown argument: $1"; usage >&2; exit 1 ;;
  esac
done

if [[ -z "$ARTIFACTS_DIR" ]]; then
  ARTIFACTS_DIR="${REPO_ROOT}/artifacts/load-test/$(date +%Y%m%d-%H%M%S)"
fi
mkdir -p "$ARTIFACTS_DIR"

cleanup() {
  local code=$?
  for pid in "${LOAD_PF_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
    # SIGTERM may not propagate if the kubectl process was orphaned; escalate.
    sleep 0.2
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  for logf in "${LOAD_PF_LOGS[@]}"; do
    [[ -f "$logf" ]] && rm -f "$logf"
  done
  exit "$code"
}
trap cleanup EXIT INT TERM

# Proactively clean up any stale kubectl port-forwards from prior runs that
# didn't tear down cleanly (e.g. script killed by SIGKILL before the EXIT
# trap ran). The pre-bind check in safe_port_forward would otherwise refuse
# to start, forcing the operator to hunt for them by hand.
kill_stale_port_forwards() {
  local pids
  pids="$(pgrep -f "kubectl .* port-forward .* 18081:18080" || true)"
  pids+=" $(pgrep -f "kubectl .* port-forward .* 18082:18080" || true)"
  pids="$(echo "$pids" | tr -s ' ' | sed -e 's/^ //; s/ $//')"
  if [[ -n "$pids" ]]; then
    log_warn "found stale kubectl port-forwards on 18081/18082 (pids: ${pids}); killing them before starting"
    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true
    sleep 0.3
    # shellcheck disable=SC2086
    kill -9 $pids 2>/dev/null || true
  fi
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

preflight_tools() {
  log_step "Load-test preflight"
  local missing=()
  for cmd in docker kubectl helm jq curl cargo node; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    log_fail "Missing required tools: ${missing[*]}"
    exit 1
  fi
  log_info "Tool preflight OK (kubectl, helm, jq, node, ...)."
}

preflight_builder() {
  if [[ -f "$BUILDER_BIN" ]]; then
    log_info "baml-agent-builder already built at ${BUILDER_BIN}."
    return
  fi
  if [[ "$SKIP_BUILDER" == "true" ]]; then
    log_fail "Builder binary not found at ${BUILDER_BIN} and --skip-builder-build was passed."
    exit 1
  fi
  log_step "Building baml-agent-builder (needed by publish_fixture)"
  (cd "$REPO_ROOT" && cargo build --release -p baml-rt-builder --bin baml-agent-builder --all-features)
  if [[ ! -f "$BUILDER_BIN" ]]; then
    log_fail "Build completed but ${BUILDER_BIN} missing — check cargo output above."
    exit 1
  fi
}

start_observability() {
  if [[ "$SKIP_OBSERVABILITY" == "true" ]]; then
    log_info "Skipping observability stack start (--skip-observability)."
    return
  fi
  if [[ ! -f "$OBSERVABILITY_COMPOSE_FILE" ]]; then
    log_fail "Observability compose file missing: ${OBSERVABILITY_COMPOSE_FILE}"
    exit 1
  fi
  log_step "Starting local observability stack (docker compose)"
  (cd "$(dirname "$OBSERVABILITY_COMPOSE_FILE")" && docker compose up -d)
  # Confirm OTLP gRPC port (4317) and Prometheus (9090) are reachable on the
  # host before continuing. Grafana (3000) is nice-to-have, not required.
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if nc -z localhost "$OTLP_HOST_PORT" 2>/dev/null \
      || node -e "require('net').createConnection({host:'localhost',port:${OTLP_HOST_PORT}}).on('connect',()=>process.exit(0)).on('error',()=>process.exit(1))" 2>/dev/null; then
      break
    fi
    sleep 0.5
  done
  if ! curl -sf -o /dev/null --connect-timeout 2 "${PROMETHEUS_URL}/-/ready"; then
    log_fail "Prometheus ${PROMETHEUS_URL}/-/ready not responding — is the observability stack healthy?"
    exit 1
  fi
  log_info "Observability stack ready (OTLP :${OTLP_HOST_PORT}, Prometheus :9090)."
}

# ---------------------------------------------------------------------------
# Cluster bringup
# ---------------------------------------------------------------------------

bringup() {
  if [[ "$SKIP_BRINGUP" == "true" ]]; then
    log_info "Skipping bringup (--skip-bringup). Assuming cluster ${CLUSTER_NAME} is installed with OTLP overlay."
    return
  fi
  log_step "Running scripts/verify-k8s-pilot-package.sh --keep-cluster --values <overlay>"
  local args=(--keep-cluster --values "$VALUES_OVERLAY")
  if (( ${#VERIFY_EXTRA_ARGS[@]} > 0 )); then
    args+=("${VERIFY_EXTRA_ARGS[@]}")
  fi
  bash "${SCRIPT_DIR}/verify-k8s-pilot-package.sh" "${args[@]}"
}

resync_state_with_cluster() {
  log_step "Resyncing wrapper shell with installed cluster"
  # The random value lib.sh assigned on source (lib.sh:16) does not match the
  # runner-token secret created by the verifier subprocess. Read the secret
  # back so publish_fixture / deploy_hash send the correct X-Runner-Token.
  local token
  if ! token="$(kubectl -n "$NAMESPACE" get secret runner-token -o jsonpath='{.data.token}' 2>/dev/null | base64 -d)"; then
    log_fail "Could not read ${NAMESPACE}/runner-token secret — is the cluster up?"
    exit 1
  fi
  if [[ -z "$token" ]]; then
    log_fail "${NAMESPACE}/runner-token secret is empty"
    exit 1
  fi
  E2E_TOKEN="$token"
  log_info "runner-token resolved from cluster secret."

  # resolve_chart_names populates RUNNER_POD_0 / RUNNER_POD_1 / SURREAL_POD_0 /
  # RUNNER_FULLNAME / RUNNER_HEADLESS_DNS in this shell (they start empty per
  # lib.sh:28-29). Must happen before the OTLP probe and port-forwards.
  resolve_chart_names
  if [[ -z "$RUNNER_POD_0" || -z "$RUNNER_POD_1" ]]; then
    log_fail "resolve_chart_names did not set RUNNER_POD_0/RUNNER_POD_1"
    exit 1
  fi
  log_info "Resolved pods: runner-0=${RUNNER_POD_0}, runner-1=${RUNNER_POD_1}, surreal-0=${SURREAL_POD_0}"
}

# ---------------------------------------------------------------------------
# OTLP reachability probe (from inside a runner pod)
# ---------------------------------------------------------------------------

probe_otlp_reachability() {
  log_step "Verifying OTLP gRPC reachability from inside ${RUNNER_POD_0}"
  # The runtime image is debian:bookworm-slim with only ca-certificates +
  # libssl3 + curl + nodejs (Dockerfile:39-48). /dev/tcp would need bash
  # (absent). Node 22 IS installed, so a plain net.createConnection is the
  # correct TCP probe. DNS-only probes like `getent` would not detect a
  # running-but-unlistening collector.
  #
  # Candidate host aliases, tried in order:
  #   host.k3d.internal     — k3d-on-Docker default (CoreDNS-injected).
  #   host.docker.internal  — Docker Desktop / Podman (aliased).
  #   host.containers.internal — Podman default.
  # The first alias whose TCP probe succeeds wins; if it differs from the
  # overlay's configured endpoint, the wrapper helm-upgrades to match.
  local candidates=("host.k3d.internal" "host.docker.internal" "host.containers.internal")
  local working=""
  for candidate in "${candidates[@]}"; do
    log_info "probing ${candidate}:4317 from ${RUNNER_POD_0}..."
    if kubectl -n "$NAMESPACE" exec "$RUNNER_POD_0" -- node -e "
      const net = require('net');
      const sock = net.createConnection({ host: '${candidate}', port: 4317 });
      const timer = setTimeout(() => { console.error('TIMEOUT'); process.exit(2); }, 3000);
      sock.on('connect', () => { clearTimeout(timer); console.log('OK'); sock.end(); process.exit(0); });
      sock.on('error', (e) => { clearTimeout(timer); console.error('FAIL ' + e.message); process.exit(1); });
    " >/dev/null 2>&1; then
      working="$candidate"
      break
    fi
  done
  if [[ -z "$working" ]]; then
    log_fail "In-pod OTLP TCP probe failed for all candidates: ${candidates[*]}."
    log_fail "Check: is the observability stack up and listening on host port 4317?"
    log_fail "  docker compose -f ${OBSERVABILITY_COMPOSE_FILE} ps"
    log_fail "  docker compose -f ${OBSERVABILITY_COMPOSE_FILE} logs otel-collector"
    log_fail "Debug from inside the pod:"
    log_fail "  kubectl -n ${NAMESPACE} exec ${RUNNER_POD_0} -- node -e '<probe with your host alias>'"
    exit 1
  fi
  OTLP_ENDPOINT="http://${working}:4317"
  log_info "Runner pods can reach ${OTLP_ENDPOINT}."
  # If the host alias we resolved differs from what Helm injected, patch the
  # StatefulSet env var so runner pods actually export OTLP.
  local current
  current="$(kubectl -n "$NAMESPACE" get sts "$RUNNER_FULLNAME" \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="OTEL_EXPORTER_OTLP_ENDPOINT")].value}')"
  if [[ "$current" != "$OTLP_ENDPOINT" ]]; then
    log_info "Updating runner OTEL_EXPORTER_OTLP_ENDPOINT (${current:-unset} -> ${OTLP_ENDPOINT}) via helm upgrade"
    helm upgrade "$RELEASE_NAME" \
      "${REPO_ROOT}/deploy/helm/agentium-os/" \
      --namespace "$NAMESPACE" \
      --reuse-values \
      --set-string "observability.otlpEndpoint=${OTLP_ENDPOINT}" >/dev/null
    kubectl -n "$NAMESPACE" rollout status "statefulset/${RUNNER_FULLNAME}" --timeout=120s
  else
    log_info "Runner StatefulSet already has OTEL_EXPORTER_OTLP_ENDPOINT=${OTLP_ENDPOINT}"
  fi
}

# ---------------------------------------------------------------------------
# Safe port-forward (matches scripts/k8s-pilot-smoke.sh:118-141)
# ---------------------------------------------------------------------------
# Does NOT use lib.sh's start_port_forward — that helper suppresses kubectl
# output and treats any /healthz success on the local port as readiness,
# which yields false positives if another process is already bound to the
# same port (notably: a concurrent just e2e-k8s run using the same 18081/
# 18082). This wrapper refuses to run if the port is pre-bound and fails
# loud with captured kubectl output if port-forward dies mid-boot.

safe_port_forward() {
  local pod="$1" local_port="$2" remote_port="$3"
  if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:${local_port}/healthz" 2>/dev/null; then
    log_fail "localhost:${local_port} already responds to /healthz — another process is bound here."
    log_fail "Stop it (e.g. a stale port-forward or concurrent e2e run) before retrying."
    exit 1
  fi
  local logf
  logf="$(mktemp -t k8s-load-pf-XXXXXX.log)"
  LOAD_PF_LOGS+=("$logf")
  kubectl -n "$NAMESPACE" port-forward "$pod" "${local_port}:${remote_port}" >"$logf" 2>&1 &
  local pid=$!
  LOAD_PF_PIDS+=("$pid")
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if ! kill -0 "$pid" 2>/dev/null; then
      log_fail "kubectl port-forward for ${pod} exited before becoming ready."
      log_fail "--- port-forward log ---"
      cat "$logf" >&2 || true
      log_fail "------------------------"
      exit 1
    fi
    if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:${local_port}/healthz" 2>/dev/null; then
      log_info "port-forward ready: localhost:${local_port} -> ${pod}:${remote_port}"
      return 0
    fi
    sleep 0.3
  done
  log_fail "port-forward for ${pod} did not become ready within 30s."
  log_fail "--- port-forward log ---"
  cat "$logf" >&2 || true
  log_fail "------------------------"
  exit 1
}

open_port_forwards() {
  log_step "Opening direct-pod port-forwards to runner-0 and runner-1"
  kill_stale_port_forwards
  safe_port_forward "$RUNNER_POD_0" "$RUNNER0_PORT" "$REMOTE_PORT"
  safe_port_forward "$RUNNER_POD_1" "$RUNNER1_PORT" "$REMOTE_PORT"
}

# ---------------------------------------------------------------------------
# Topology metadata
# ---------------------------------------------------------------------------

write_topology_json() {
  local git_sha
  git_sha="$(cd "$REPO_ROOT" && git rev-parse HEAD 2>/dev/null || echo unknown)"
  local out="${ARTIFACTS_DIR}/topology.json"
  local runner0_dns="${RUNNER_POD_0}.${RUNNER_HEADLESS_DNS}:${RUNNER_CONTAINER_PORT}"
  local runner1_dns="${RUNNER_POD_1}.${RUNNER_HEADLESS_DNS}:${RUNNER_CONTAINER_PORT}"
  jq -n \
    --arg release "$RELEASE_NAME" \
    --arg namespace "$NAMESPACE" \
    --arg image_repo "$IMAGE_NAME" \
    --arg image_tag "$IMAGE_TAG" \
    --arg runner_pod_0 "$RUNNER_POD_0" \
    --arg runner_pod_1 "$RUNNER_POD_1" \
    --arg runner_0_dns "$runner0_dns" \
    --arg runner_1_dns "$runner1_dns" \
    --arg values_overlay "$VALUES_OVERLAY" \
    --arg otlp_endpoint "$OTLP_ENDPOINT" \
    --arg prometheus_url "$PROMETHEUS_URL" \
    --arg git_sha "$git_sha" \
    --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{
      release: $release,
      namespace: $namespace,
      image: { repository: $image_repo, tag: $image_tag },
      runners: [
        { pod: $runner_pod_0, in_cluster_dns: $runner_0_dns },
        { pod: $runner_pod_1, in_cluster_dns: $runner_1_dns }
      ],
      helm_values_overlay: $values_overlay,
      otlp_endpoint: $otlp_endpoint,
      prometheus_url: $prometheus_url,
      git_sha: $git_sha,
      otlp_probe_result: "OK",
      timestamp: $timestamp
    }' >"$out"
  log_info "topology.json -> ${out}" >&2
  echo "$out"
}

# ---------------------------------------------------------------------------
# Prometheus helpers
# ---------------------------------------------------------------------------

prom_query() {
  local query="$1"
  local encoded
  encoded=$(jq -rn --arg q "$query" '$q|@uri')
  curl -sf --max-time 5 "${PROMETHEUS_URL}/api/v1/query?query=${encoded}" 2>/dev/null \
    || echo '{"status":"error"}'
}

prom_scalar_sum() {
  local query="$1"
  local raw
  raw="$(prom_query "$query")"
  echo "$raw" | jq -r '
    if .status == "success" then
      (.data.result // []) | map(.value[1] | tonumber? // 0) | add // 0
    else
      0
    end'
}

prom_serving_breakdown() {
  local pkg="$1"
  local raw
  raw="$(prom_query "sum by (serving_service_instance_id) (baml_rt_a2a_request_total{agent_package=\"${pkg}\"})")"
  echo "$raw" | jq -c '
    if .status == "success" then
      [(.data.result // [])[] | { key: .metric.serving_service_instance_id, value: (.value[1] | tonumber) }]
      | from_entries
    else
      {}
    end'
}

# Poll Prometheus until the most recent sample timestamp for the runner A2A
# series is newer than the given epoch (typically the load-start time), so
# deltas read against a scrape that actually covers the just-finished run.
# Falls back after 30s with a warning rather than blocking forever.
wait_for_prom_scrape_since() {
  local since_epoch="$1"
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    local raw max_ts
    raw="$(prom_query "baml_rt_a2a_request_total{agent_package=\"${PACKAGE}\"}")"
    max_ts="$(echo "$raw" | jq -r '[.data.result[]?.value[0]] | max // 0' | cut -d. -f1)"
    if [[ -n "$max_ts" && "$max_ts" -ge "$since_epoch" ]]; then
      return 0
    fi
    sleep 2
  done
  log_warn "Prometheus scrape did not refresh within 30s of load end (since=${since_epoch}); deltas may be stale"
}


# Send one curl probe per ingress target to confirm the deploy is live and the
# response contains the expected substring. Runs in the wrapper (not inside
# run.mjs) so the wrapper can snapshot Prometheus *after* these probes have
# been scraped — otherwise the smoke traffic lands in the per-scenario
# serving delta and can by itself satisfy the split_dual_runner shape check.
wrapper_smoke() {
  local ingress_csv="$1"
  local -a bases
  IFS=',' read -ra bases <<< "$ingress_csv"
  local i=0 stamp base target corr_id msg_id body response
  stamp="$(date +%s)"
  for base in "${bases[@]}"; do
    i=$((i + 1))
    corr_id="corr-${stamp}-${i}"
    msg_id="m-${stamp}-${i}"
    target="${base%/}/agents/${PACKAGE}/${INSTANCE}/a2a"
    body=$(jq -n \
      --arg corr "$corr_id" --arg msg "$msg_id" \
      --arg text "$PAYLOAD" \
      '{jsonrpc:"2.0",id:$corr,method:"message.sendStream",
        params:{message:{messageId:$msg,role:"user",parts:[{kind:"text",text:$text}]}}}')
    response="$(curl -sf --max-time 30 -X POST \
      -H 'Content-Type: application/json' -d "$body" "$target" 2>&1 \
      || echo '__CURL_FAILED__')"
    if [[ "$response" != *"$EXPECT_SUBSTRING"* ]]; then
      log_fail "smoke request to ${target} did not return expected substring"
      log_fail "response: ${response:0:300}"
      return 1
    fi
    log_info "smoke ok: ${target}"
  done
}

# ---------------------------------------------------------------------------
# Per-scenario setup
# ---------------------------------------------------------------------------

setup_scenario_local_a2a() {
  log_info "scenario setup: deploy dispatch-echo on runner-0 only" >&2
  publish_and_deploy "$PACKAGE" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null 2>&1
  echo "http://localhost:${RUNNER0_PORT}"
}

setup_scenario_forwarded_a2a() {
  log_info "scenario setup: deploy dispatch-echo on runner-1 only; publish metadata to runner-0" >&2
  publish_and_deploy "$PACKAGE" "$RUNNER1_PORT" "$E2E_TOKEN" >/dev/null 2>&1
  publish_fixture "$PACKAGE" "$RUNNER0_PORT" >/dev/null 2>&1
  echo "http://localhost:${RUNNER0_PORT}"
}

setup_scenario_split_dual_runner() {
  log_info "scenario setup: deploy dispatch-echo on both runners" >&2
  publish_and_deploy "$PACKAGE" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null 2>&1
  publish_and_deploy "$PACKAGE" "$RUNNER1_PORT" "$E2E_TOKEN" >/dev/null 2>&1
  echo "http://localhost:${RUNNER0_PORT},http://localhost:${RUNNER1_PORT}"
}

teardown_scenario() {
  local rc=0
  teardown_runner "runner-0" "$RUNNER0_PORT" || rc=1
  teardown_runner "runner-1" "$RUNNER1_PORT" || rc=1
  return "$rc"
}

teardown_runner() {
  local runner_label="$1" port="$2"
  local agents_json hash
  if ! agents_json="$(curl -sf "http://localhost:${port}/agents")"; then
    log_warn "teardown: could not query /agents on ${runner_label} (localhost:${port})"
    return 1
  fi

  hash="$(echo "$agents_json" | jq -r --arg pkg "$PACKAGE" '.[] | select(.agent_package == $pkg) | .agent_card.content_hash // empty' | head -1)"
  if [[ -z "$hash" ]]; then
    log_info "teardown: ${PACKAGE} already absent on ${runner_label}"
    return 0
  fi

  if ! curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${port}/undeploy" >/dev/null; then
    log_warn "teardown: undeploy request failed for ${PACKAGE} on ${runner_label} (hash=${hash})"
    return 1
  fi

  if ! agents_json="$(curl -sf "http://localhost:${port}/agents")"; then
    log_warn "teardown: undeploy request completed but /agents recheck failed on ${runner_label}"
    return 1
  fi
  if echo "$agents_json" | jq -e --arg pkg "$PACKAGE" '.[] | select(.agent_package == $pkg)' >/dev/null 2>&1; then
    log_warn "teardown: ${PACKAGE} still listed on ${runner_label} after undeploy"
    return 1
  fi

  log_info "teardown: removed ${PACKAGE} from ${runner_label}"
}

# ---------------------------------------------------------------------------
# Run a single scenario end-to-end
# ---------------------------------------------------------------------------

run_scenario() {
  local scenario="$1" topology_path="$2"
  log_step "Scenario: ${scenario}"

  local setup_fn=""
  case "$scenario" in
    local_a2a)         setup_fn="setup_scenario_local_a2a" ;;
    forwarded_a2a)     setup_fn="setup_scenario_forwarded_a2a" ;;
    split_dual_runner) setup_fn="setup_scenario_split_dual_runner" ;;
    *) log_fail "unknown scenario: ${scenario}"; return 1 ;;
  esac

  local had_errexit=false
  [[ $- == *e* ]] && had_errexit=true
  set +e

  local ingress_csv=""
  local scenario_rc=0
  local node_rc=0
  local assert_rc=0
  local teardown_rc=0
  local have_pre_snapshot=false

  local smoke_epoch=""
  local run_started_epoch=""
  local pre_forward="0"
  local pre_request="0"
  local pre_serving="{}"
  local post_forward="0"
  local post_request="0"
  local post_serving="{}"
  local forward_delta="0.000"
  local request_delta="0.000"
  local serving_delta="{}"
  local obs_json=""

  # Handle scenario status explicitly so teardown behaves like `finally`
  # even when the load runner exits non-zero under the script's global
  # `set -e`.
  ingress_csv="$("$setup_fn")"
  scenario_rc=$?

  if (( scenario_rc == 0 )); then
    # Smoke first (fail-fast on misconfig), then wait for those smoke requests
    # to appear in Prometheus before taking the pre-snapshot — otherwise the
    # smoke traffic ends up in the measured-load delta and can by itself
    # satisfy the split_dual_runner shape check.
    smoke_epoch="$(date +%s)"
    wrapper_smoke "$ingress_csv"
    scenario_rc=$?
  fi

  if (( scenario_rc == 0 )); then
    wait_for_prom_scrape_since "$smoke_epoch"

    # Snapshot per-runner serving counts pre/post so assertions isolate this
    # scenario's measured traffic from earlier scenarios (cumulative) and
    # from the smoke we just sent.
    pre_forward="$(prom_scalar_sum "sum(baml_rt_cluster_a2a_forward_total{agent_package=\"${PACKAGE}\"})")"
    pre_request="$(prom_scalar_sum "sum(baml_rt_a2a_request_total{agent_package=\"${PACKAGE}\"})")"
    pre_serving="$(prom_serving_breakdown "$PACKAGE")"
    run_started_epoch="$(date +%s)"
    have_pre_snapshot=true

    node "${REPO_ROOT}/scripts/load-test/run.mjs" \
      --scenario "$scenario" \
      --ingress "$ingress_csv" \
      --package "$PACKAGE" \
      --instance "$INSTANCE" \
      --concurrency "$CONCURRENCY" \
      --warmup-seconds "$WARMUP_SECONDS" \
      --measured-seconds "$MEASURED_SECONDS" \
      --payload "$PAYLOAD" \
      --expect-substring "$EXPECT_SUBSTRING" \
      --artifacts-dir "$ARTIFACTS_DIR" \
      --topology-json "$topology_path"
    node_rc=$?
    if (( node_rc != 0 )); then
      scenario_rc=$node_rc
      log_fail "scenario ${scenario}: load runner exited with status ${node_rc}"
    fi
  fi

  if [[ "$have_pre_snapshot" == "true" ]]; then
    wait_for_prom_scrape_since "$run_started_epoch"

    post_forward="$(prom_scalar_sum "sum(baml_rt_cluster_a2a_forward_total{agent_package=\"${PACKAGE}\"})")"
    post_request="$(prom_scalar_sum "sum(baml_rt_a2a_request_total{agent_package=\"${PACKAGE}\"})")"
    post_serving="$(prom_serving_breakdown "$PACKAGE")"
    forward_delta="$(awk -v a="$post_forward" -v b="$pre_forward" 'BEGIN{printf "%.3f", a-b}')"
    request_delta="$(awk -v a="$post_request" -v b="$pre_request" 'BEGIN{printf "%.3f", a-b}')"
    # `$pre + $post` is used only to enumerate the union of keys; values come
    # from indexing $pre / $post directly in the reduce body.
    serving_delta="$(jq -n --argjson pre "$pre_serving" --argjson post "$post_serving" '
      reduce ((($pre // {}) + ($post // {})) | keys[]) as $k
        ({}; . + { ($k): (($post[$k] // 0) - ($pre[$k] // 0)) })
      | with_entries(select(.value != 0))
    ')"
    if [[ $? -ne 0 ]]; then
      if (( scenario_rc == 0 )); then
        scenario_rc=1
      fi
      log_fail "scenario ${scenario}: could not compute per-runner serving delta"
    else
      # `baml_rt_a2a_request_total_by_serving` (cumulative) is retained for
      # operator diagnostics; `..._by_serving_delta` is what assertions use.
      obs_json="$(jq -n \
        --argjson forward_delta "$forward_delta" \
        --argjson request_delta "$request_delta" \
        --argjson serving_cumulative "$post_serving" \
        --argjson serving_delta "$serving_delta" \
        --arg prom_url "$PROMETHEUS_URL" \
        '{
          prometheus_url: $prom_url,
          baml_rt_cluster_a2a_forward_total_delta: $forward_delta,
          baml_rt_a2a_request_total_delta: $request_delta,
          baml_rt_a2a_request_total_by_serving: $serving_cumulative,
          baml_rt_a2a_request_total_by_serving_delta: $serving_delta
        }')"
      if [[ $? -ne 0 ]]; then
        if (( scenario_rc == 0 )); then
          scenario_rc=1
        fi
        log_fail "scenario ${scenario}: could not build observability payload"
      else
        # Splice the observability block into the scenario JSON the Node harness wrote.
        local scenario_json="${ARTIFACTS_DIR}/${scenario}.json"
        if [[ -f "$scenario_json" ]]; then
          local splice_rc=0
          jq --argjson obs "$obs_json" '. + { observability: $obs }' "$scenario_json" >"${scenario_json}.tmp"
          if [[ $? -eq 0 ]]; then
            mv "${scenario_json}.tmp" "$scenario_json"
            splice_rc=$?
          else
            splice_rc=1
          fi
          if (( splice_rc != 0 )); then
            if (( scenario_rc == 0 )); then
              scenario_rc=1
            fi
            log_warn "could not splice observability block into ${scenario_json}"
          fi
        else
          log_warn "Node harness did not write ${scenario_json}"
        fi

        SCENARIO_RESULTS_JSON+=("$(jq -n --arg scenario "$scenario" --argjson forward_delta "$forward_delta" --argjson request_delta "$request_delta" --argjson serving_delta "$serving_delta" '{scenario:$scenario, forward_delta:$forward_delta, request_delta:$request_delta, serving_delta:$serving_delta}')")
      fi

      if (( node_rc == 0 )); then
        assert_scenario_shape "$scenario" "$forward_delta" "$request_delta" "$serving_delta"
        assert_rc=$?
        if (( assert_rc != 0 )); then
          scenario_rc=$assert_rc
        fi
      else
        log_warn "scenario ${scenario}: skipping scenario-shape assertion because the load runner failed first (rc=${node_rc})"
      fi
    fi
  fi

  teardown_scenario
  teardown_rc=$?

  if $had_errexit; then
    set -e
  fi

  if (( teardown_rc != 0 )); then
    if (( scenario_rc != 0 )); then
      log_warn "scenario ${scenario}: teardown reported cleanup errors after earlier failure (rc=${scenario_rc}); returning the original scenario failure"
      return "$scenario_rc"
    fi
    log_fail "scenario ${scenario}: teardown reported cleanup errors"
    return "$teardown_rc"
  fi

  return "$scenario_rc"
}

assert_scenario_shape() {
  local scenario="$1" forward_delta="$2" request_delta="$3" serving_delta="$4"
  # serving_delta is {<service.instance.id>: <requests_this_scenario>} with
  # zero-delta entries filtered out. service.instance.id equals the pod name
  # in the pilot (see resource attributes wired by runner-statefulset.yaml),
  # so `$RUNNER_POD_0` and `$RUNNER_POD_1` are the expected keys.
  local actual_keys
  actual_keys="$(echo "$serving_delta" | jq -r 'keys | sort | join(",")')"
  local r0_count r1_count
  r0_count="$(echo "$serving_delta" | jq --arg k "$RUNNER_POD_0" '.[$k] // 0')"
  r1_count="$(echo "$serving_delta" | jq --arg k "$RUNNER_POD_1" '.[$k] // 0')"
  local ok=true
  case "$scenario" in
    local_a2a)
      if awk -v d="$forward_delta" 'BEGIN{exit !(d > 0.5)}'; then
        log_fail "local_a2a: expected forward_delta ≈ 0, got ${forward_delta}"
        ok=false
      fi
      if [[ "$actual_keys" != "$RUNNER_POD_0" ]]; then
        log_fail "local_a2a: expected serving_delta keys == [${RUNNER_POD_0}], got [${actual_keys}]. Delta: ${serving_delta}"
        ok=false
      fi
      if $ok; then
        log_info "local_a2a: no forward traffic; served only by ${RUNNER_POD_0} (count=${r0_count}). OK."
      fi
      ;;
    forwarded_a2a)
      if awk -v d="$forward_delta" 'BEGIN{exit !(d < 0.5)}'; then
        log_fail "forwarded_a2a: expected forward_delta to grow, got ${forward_delta}"
        ok=false
      fi
      if [[ "$actual_keys" != "$RUNNER_POD_1" ]]; then
        log_fail "forwarded_a2a: expected serving_delta keys == [${RUNNER_POD_1}] (peer only), got [${actual_keys}]. Delta: ${serving_delta}"
        ok=false
      fi
      if $ok; then
        log_info "forwarded_a2a: forward_delta=${forward_delta}, request_delta=${request_delta}; served only by ${RUNNER_POD_1} (count=${r1_count}). OK."
      fi
      ;;
    split_dual_runner)
      if ! awk -v c="$r0_count" 'BEGIN{exit !(c > 0)}'; then
        log_fail "split_dual_runner: expected ${RUNNER_POD_0} to serve traffic, got ${r0_count}. Delta: ${serving_delta}"
        ok=false
      fi
      if ! awk -v c="$r1_count" 'BEGIN{exit !(c > 0)}'; then
        log_fail "split_dual_runner: expected ${RUNNER_POD_1} to serve traffic, got ${r1_count}. Delta: ${serving_delta}"
        ok=false
      fi
      if $ok; then
        log_info "split_dual_runner: ${RUNNER_POD_0}=${r0_count}, ${RUNNER_POD_1}=${r1_count}. OK."
      fi
      ;;
  esac
  if ! $ok; then
    return 1
  fi
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

write_summary() {
  local summary_json="${ARTIFACTS_DIR}/summary.json"
  local summary_md="${ARTIFACTS_DIR}/summary.md"

  local scenarios_array="[]"
  if (( ${#SCENARIO_RESULTS_JSON[@]} > 0 )); then
    scenarios_array="$(printf '%s\n' "${SCENARIO_RESULTS_JSON[@]}" | jq -s '.')"
  fi

  jq -n \
    --arg artifacts_dir "$ARTIFACTS_DIR" \
    --argjson scenarios "$scenarios_array" \
    '{artifacts_dir: $artifacts_dir, scenarios: $scenarios}' >"$summary_json"

  {
    echo "# K8s pilot load-test summary"
    echo
    echo "- Artifacts: \`${ARTIFACTS_DIR}\`"
    echo "- Package: \`${PACKAGE}\` (instance \`${INSTANCE}\`)"
    echo "- Workload: warmup ${WARMUP_SECONDS}s, measured ${MEASURED_SECONDS}s, concurrency \`${CONCURRENCY}\`"
    echo "- OTLP: \`${OTLP_ENDPOINT}\` (verified via in-pod node TCP probe)"
    echo
    echo "## Per-scenario latency"
    echo
    for scenario in local_a2a forwarded_a2a split_dual_runner; do
      local path="${ARTIFACTS_DIR}/${scenario}.json"
      [[ -f "$path" ]] || continue
      echo "### ${scenario}"
      echo
      echo "| concurrency | ok/total | rps | p50 ms | p95 ms | p99 ms | max ms |"
      echo "|-------------|----------|-----|--------|--------|--------|--------|"
      jq -r '
        .concurrency_levels[] |
        "| \(.concurrency) | \(.success)/\(.total) | \(.throughputRps) | \(.timeToResponseCompleteMs.p50) | \(.timeToResponseCompleteMs.p95) | \(.timeToResponseCompleteMs.p99) | \(.timeToResponseCompleteMs.max) |"
      ' "$path"
      echo
      echo "Observability (Prometheus counter deltas for this scenario):"
      jq -r '
        "- baml_rt_cluster_a2a_forward_total delta: \(.observability.baml_rt_cluster_a2a_forward_total_delta // "n/a")",
        "- baml_rt_a2a_request_total delta: \(.observability.baml_rt_a2a_request_total_delta // "n/a")",
        "- serving delta (this scenario only): \(.observability.baml_rt_a2a_request_total_by_serving_delta // {} | @json)",
        "- serving cumulative (since runner start): \(.observability.baml_rt_a2a_request_total_by_serving // {} | @json)"
      ' "$path"
      echo
    done
  } >"$summary_md"

  log_info "summary.json -> ${summary_json}"
  log_info "summary.md   -> ${summary_md}"
  echo
  cat "$summary_md"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  preflight_tools
  preflight_builder
  start_observability
  bringup
  resync_state_with_cluster
  probe_otlp_reachability
  open_port_forwards
  local topology_path
  topology_path="$(write_topology_json)"

  local IFS=,
  for scenario in $SCENARIOS; do
    case "$scenario" in
      local_a2a|forwarded_a2a|split_dual_runner)
        run_scenario "$scenario" "$topology_path"
        ;;
      *)
        log_fail "unknown scenario in --scenarios: ${scenario}"
        exit 1
        ;;
    esac
  done
  unset IFS

  write_summary
  log_step "Load-test PASSED"
  log_info "Artifacts: ${ARTIFACTS_DIR}"
}

main "$@"
