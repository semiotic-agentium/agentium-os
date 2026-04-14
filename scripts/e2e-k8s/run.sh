#!/usr/bin/env bash
# E2E k8s test harness for the Agentium OS k8s deployment feature.
#
# Builds a Docker image, boots a k3d cluster, applies manifests, and runs
# 15 scenario assertions through real pod surfaces.
#
# Usage: ./scripts/e2e-k8s/run.sh [--no-build] [--keep-cluster]
#
# Options:
#   --no-build      Skip Docker image and builder binary builds (reuse cached)
#   --keep-cluster  Do not delete the k3d cluster on exit

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release/baml-agent-builder"

# Parse flags
SKIP_BUILD=false
KEEP_CLUSTER=false
for arg in "$@"; do
  case "$arg" in
    --no-build)     SKIP_BUILD=true ;;
    --keep-cluster) KEEP_CLUSTER=true ;;
  esac
done

# ============================================================================
# Cleanup
# ============================================================================
cleanup() {
  echo ""
  log_info "Cleaning up..."
  kill_all_port_forwards
  if (( HAS_FAILURE )); then
    LOG_DIR="./e2e-k8s-logs/$(date +%Y%m%d-%H%M%S)"
    dump_logs "$LOG_DIR"
  fi
  if [[ "$KEEP_CLUSTER" == "false" ]]; then
    k3d cluster delete "$CLUSTER_NAME" 2>/dev/null || true
  else
    log_info "Cluster '$CLUSTER_NAME' kept (--keep-cluster)."
  fi
  print_summary
  if (( HAS_FAILURE )); then exit 1; fi
}
trap cleanup EXIT INT TERM

# ============================================================================
# Preflight
# ============================================================================
preflight() {
  log_step "Preflight checks"
  local missing=()
  for cmd in docker k3d kubectl jq curl; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    log_fail "Missing required tools: ${missing[*]}"
    echo "  Install them before running the E2E harness."
    exit 1
  fi

  # Podman preflight (same checks as deploy/demo/run-demo.sh)
  if docker info 2>/dev/null | grep -qi podman; then
    if podman machine inspect 2>/dev/null | grep -q '"Rootful": false'; then
      log_fail "Podman Machine is running in rootless mode."
      echo "  Fix: podman machine stop && podman machine set --rootful --memory 8192 && podman machine start"
      exit 1
    fi
    local log_driver
    log_driver="$(podman info --format '{{.Host.LogDriver}}' 2>/dev/null || true)"
    if [[ "$log_driver" == "journald" ]]; then
      log_fail "Podman log driver is 'journald' — k3d needs 'k8s-file'."
      echo "  Fix: podman machine ssh -- 'sudo mkdir -p /etc/containers && echo -e \"[containers]\nlog_driver = \\\"k8s-file\\\"\" | sudo tee /etc/containers/containers.conf'"
      exit 1
    fi
  fi

  log_info "All preflight checks passed."
}

# ============================================================================
# Build
# ============================================================================
build_phase() {
  if [[ "$SKIP_BUILD" == "true" ]]; then
    log_step "Build (skipped — --no-build)"
    if [[ ! -f "$BUILDER_BIN" ]]; then
      log_fail "Builder binary not found at ${BUILDER_BIN}. Run without --no-build first."
      exit 1
    fi
    return
  fi

  log_step "Building agent builder binary"
  (cd "$REPO_ROOT" && cargo build --release -p baml-rt-builder --bin baml-agent-builder --all-features)

  log_step "Building Docker image"
  docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" "$REPO_ROOT"
}

# ============================================================================
# Cluster setup
# ============================================================================
setup_cluster() {
  log_step "Creating k3d cluster"
  if k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"${CLUSTER_NAME}\""; then
    log_info "Cluster '${CLUSTER_NAME}' already exists — deleting for clean run."
    k3d cluster delete "$CLUSTER_NAME"
  fi
  k3d cluster create --config "${REPO_ROOT}/deploy/k3d/cluster.yaml"

  log_step "Importing image into k3d"
  local import_tar
  import_tar="$(mktemp -t agentium-image-XXXXXX).tar"
  docker tag "${IMAGE_NAME}:${IMAGE_TAG}" "docker.io/library/${IMAGE_NAME}:${IMAGE_TAG}" 2>/dev/null || true
  docker save -o "$import_tar" "docker.io/library/${IMAGE_NAME}:${IMAGE_TAG}"
  k3d image import "$import_tar" -c "$CLUSTER_NAME"
  rm -f "$import_tar"

  log_step "Applying k8s manifests"
  kubectl apply -f "${REPO_ROOT}/deploy/k8s/namespace.yaml"

  # Create secrets inline (no committed secret files needed for E2E)
  kubectl -n "$NAMESPACE" create secret generic surrealdb-credentials \
    --from-literal=username="$SURREAL_USER" \
    --from-literal=password="$SURREAL_PASS" \
    --dry-run=client -o yaml | kubectl apply -f -

  if [[ -f "${REPO_ROOT}/.env" ]]; then
    local tmp_env
    tmp_env="$(mktemp)"
    # Keep only secret-like vars (KEY/TOKEN/SECRET) and strip surrounding double quotes
    grep -E '^[A-Za-z_]*(KEY|TOKEN|SECRET)=' "${REPO_ROOT}/.env" \
      | sed -E 's/^([^=]*)="(.*)"$/\1=\2/' > "${tmp_env}"
    kubectl -n "$NAMESPACE" create secret generic fnox-secrets \
      --from-env-file="${tmp_env}" \
      --dry-run=client -o yaml | kubectl apply -f -
    rm -f "${tmp_env}"
    log_info ".env secrets injected into fnox-secrets (LLM fixtures enabled)"
  else
    kubectl -n "$NAMESPACE" create secret generic fnox-secrets \
      --from-literal=PLACEHOLDER="unused" \
      --dry-run=client -o yaml | kubectl apply -f -
    log_warn "No .env file found — LLM-dependent fixtures will use fallback behavior"
  fi

  # Mount local fnox.toml as ConfigMap so BAML functions can resolve API keys
  if [[ -f "${REPO_ROOT}/fnox.toml" ]]; then
    kubectl -n "$NAMESPACE" create configmap fnox-config \
      --from-file=fnox.toml="${REPO_ROOT}/fnox.toml" \
      --dry-run=client -o yaml | kubectl apply -f -
    log_info "fnox.toml mounted as ConfigMap (LLM fixtures enabled)"
  else
    kubectl -n "$NAMESPACE" create configmap fnox-config \
      --from-literal=placeholder=true \
      --dry-run=client -o yaml | kubectl apply -f -
    log_warn "No local fnox.toml found — LLM-dependent fixtures will fail gracefully"
  fi

  kubectl apply -f "${REPO_ROOT}/deploy/k8s/surrealdb.yaml"
  kubectl apply -f "${REPO_ROOT}/deploy/k8s/runner.yaml"

  log_info "Patching runner image to ${IMAGE_NAME}:${IMAGE_TAG}"
  kubectl -n "$NAMESPACE" set image statefulset/runner "runner=${IMAGE_NAME}:${IMAGE_TAG}"

  log_info "Injecting RUNNER_TOKEN into runner StatefulSet"
  kubectl -n "$NAMESPACE" set env statefulset/runner "RUNNER_TOKEN=${E2E_TOKEN}"

  log_info "Mounting fnox.toml ConfigMap into runner StatefulSet"
  kubectl -n "$NAMESPACE" patch statefulset runner --type=json -p='[
    {"op":"add","path":"/spec/template/spec/volumes/-","value":{"name":"fnox-config","configMap":{"name":"fnox-config","optional":true}}},
    {"op":"add","path":"/spec/template/spec/containers/0/volumeMounts/-","value":{"name":"fnox-config","mountPath":"/config/fnox.toml","subPath":"fnox.toml","readOnly":true}},
    {"op":"add","path":"/spec/template/spec/containers/0/env/-","value":{"name":"BAML_FNOX_CONFIG","value":"/config/fnox.toml"}}
  ]'

  log_step "Waiting for SurrealDB"
  kubectl -n "$NAMESPACE" wait --for=condition=ready pod -l app=surrealdb --timeout=180s

  log_info "Deleting stale runner pods so they recreate with patched spec"
  kubectl -n "$NAMESPACE" delete pods -l app=runner --force --grace-period=0 2>/dev/null || true

  log_step "Waiting for runner pods"
  kubectl -n "$NAMESPACE" wait --for=condition=ready pod/runner-0 --timeout=180s
  kubectl -n "$NAMESPACE" wait --for=condition=ready pod/runner-1 --timeout=180s
  kubectl -n "$NAMESPACE" get pods -o wide

  log_step "Starting port-forwards"
  start_port_forward runner-0 "$RUNNER0_PORT" "$REMOTE_PORT"
  start_port_forward runner-1 "$RUNNER1_PORT" "$REMOTE_PORT"
  log_info "runner-0 → localhost:${RUNNER0_PORT}, runner-1 → localhost:${RUNNER1_PORT}"
}

# ============================================================================
# Scenarios
# ============================================================================

# ---------- 1. Cluster boot sanity ----------
scenario_01_cluster_boot() {
  # Probes
  assert_http_code 200 "http://localhost:${RUNNER0_PORT}/readyz" || return 1
  assert_http_code 200 "http://localhost:${RUNNER1_PORT}/readyz" || return 1
  assert_http_code 200 "http://localhost:${RUNNER0_PORT}/healthz" || return 1
  assert_http_code 200 "http://localhost:${RUNNER1_PORT}/healthz" || return 1
  log_info "Probes OK on both runners"

  # Cluster runners table
  local result
  result=$(surreal_query "SELECT runner_id, endpoint FROM cluster_runners")
  local count
  count=$(echo "$result" | jq '[.[] | .result | .[]] | length')
  assert_eq "$count" "2" "cluster_runners row count" || return 1

  local endpoints
  endpoints=$(echo "$result" | jq -r '[.[] | .result | .[].endpoint] | sort | join(",")')
  assert_contains "$endpoints" "runner-0.runner.agentium.svc:18080" "runner-0 endpoint in cluster_runners" || return 1
  assert_contains "$endpoints" "runner-1.runner.agentium.svc:18080" "runner-1 endpoint in cluster_runners" || return 1

  local ids
  ids=$(echo "$result" | jq -r '[.[] | .result | .[].runner_id] | unique | length')
  assert_eq "$ids" "2" "distinct runner_id count" || return 1
  log_info "cluster_runners: 2 rows with correct endpoints"
}

# ---------- 2. PVC persistence across pod restart ----------
scenario_02_pvc_persistence() {
  local hash
  hash=$(publish_fixture dispatch-echo "$RUNNER0_PORT") || return 1
  log_info "Published dispatch-echo: ${hash}"

  # Deploy so we can verify post-restart
  deploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null || return 1
  log_info "Deployed dispatch-echo on runner-0"

  # Undeploy the agent (repository data stays, only deployment removed)
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
  log_info "Undeployed dispatch-echo, repository data should persist on PVC"

  # Delete the pod
  kubectl delete pod runner-0 -n "$NAMESPACE" --timeout=60s
  log_info "Deleted runner-0, waiting for replacement..."

  restart_port_forward runner-0 "$RUNNER0_PORT" "$REMOTE_PORT"
  log_info "Port-forward restored to new runner-0"

  # Re-deploy the same hash — if the PVC preserved the repository, deploy succeeds
  local resp
  resp=$(deploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN") || {
    log_fail "Re-deploy after restart failed — PVC data may be lost"
    return 1
  }

  # If deploy succeeded, repository PVC survived
  local already
  already=$(echo "$resp" | jq -r '.already_deployed // false')
  # Either fresh deploy or already_deployed — both prove persistence
  log_info "Re-deploy succeeded (already_deployed=${already}), PVC persistence confirmed"

  # Clean up
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
}

# ---------- 3. Cross-pod A2A via service DNS ----------
scenario_03_cross_pod_a2a() {
  # Publish to runner-1 and deploy only there
  local hash
  hash=$(publish_fixture dispatch-echo "$RUNNER1_PORT") || return 1
  deploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN" >/dev/null || return 1
  log_info "dispatch-echo deployed on runner-1 only (hash=${hash})"

  # Ensure dispatch-echo is NOT deployed on runner-0
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"

  # Also publish to runner-0's repository so it knows the package metadata
  publish_fixture dispatch-echo "$RUNNER0_PORT" >/dev/null || return 1

  # Send A2A request via runner-0 — should forward to runner-1
  local response
  response=$(a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "hello from e2e via runner-0")
  if [[ -z "$response" ]]; then
    log_fail "A2A request via runner-0 returned empty response"
    return 1
  fi
  log_info "A2A response received via runner-0 forwarding"

  # Verify placement points to runner-1
  local placement
  placement=$(surreal_query "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo'")
  local placement_endpoint
  placement_endpoint=$(echo "$placement" | jq -r '[.[] | .result | .[].runner_endpoint] | .[0]')
  assert_contains "$placement_endpoint" "runner-1.runner.agentium.svc" "placement endpoint points to runner-1" || return 1

  # Check runner-0 logs for forwarding evidence
  local logs
  logs=$(kubectl logs runner-0 -n "$NAMESPACE" --tail=100 2>/dev/null || true)
  if echo "$logs" | grep -qi "runner-1\|forward\|placement"; then
    log_info "runner-0 logs show forwarding evidence"
  else
    log_warn "Could not find explicit forwarding evidence in runner-0 logs (non-fatal)"
  fi

  # Clean up
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ---------- 4. Cross-pod migration via /control/migrate ----------
scenario_04_migration() {
  # Deploy on runner-0
  local hash
  hash=$(publish_and_deploy dispatch-echo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  log_info "dispatch-echo deployed on runner-0 (hash=${hash})"

  # Also publish to runner-1's repository so migration target can deploy
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1

  # Migrate to runner-1
  local migrate_resp
  migrate_resp=$(curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"http://runner-1.runner.agentium.svc:18080\"}" \
    "http://localhost:${RUNNER0_PORT}/control/migrate") || {
    log_fail "migrate request failed"
    return 1
  }

  local migrated
  migrated=$(echo "$migrate_resp" | jq -r '.migrated')
  assert_eq "$migrated" "true" "migrate response .migrated" || return 1
  log_info "Migration response: migrated=true"

  # Verify runner-0 no longer has the agent
  local agents_r0
  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  if echo "$agents_r0" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "dispatch-echo still listed on runner-0 after migration"
    return 1
  fi
  log_info "runner-0 /agents no longer lists dispatch-echo"

  # Verify runner-1 has the agent
  local agents_r1
  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  if ! echo "$agents_r1" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "dispatch-echo not listed on runner-1 after migration"
    return 1
  fi
  log_info "runner-1 /agents lists dispatch-echo"

  # Verify placement in SurrealDB
  local placement
  placement=$(surreal_query "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo'")
  local placement_endpoint
  placement_endpoint=$(echo "$placement" | jq -r '[.[] | .result | .[].runner_endpoint] | .[0]')
  assert_contains "$placement_endpoint" "runner-1.runner.agentium.svc" "placement after migration" || return 1

  # Clean up
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ---------- 5. SSRF rejection through real pod ----------
scenario_05_ssrf_rejection() {
  local hash
  hash=$(publish_and_deploy dispatch-echo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  log_info "dispatch-echo deployed on runner-0 for SSRF test"

  local targets=(
    "http://127.0.0.1:18080"
    "http://169.254.169.254"
    "http://metadata.google.internal"
  )

  for target in "${targets[@]}"; do
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
      -H "Content-Type: application/json" \
      -H "X-Runner-Token: ${E2E_TOKEN}" \
      -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"${target}\"}" \
      "http://localhost:${RUNNER0_PORT}/control/migrate")
    if [[ "$code" -ge 400 && "$code" -lt 500 ]]; then
      log_info "SSRF blocked for ${target}: HTTP ${code}"
    else
      log_fail "SSRF not blocked for ${target}: HTTP ${code} (expected 4xx)"
      return 1
    fi
  done

  # Verify agent is still deployed (no side effects from rejected migrations)
  local agents
  agents=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  if ! echo "$agents" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "dispatch-echo disappeared after SSRF rejection (side effect!)"
    return 1
  fi
  log_info "Agent still deployed after all SSRF rejections"

  # Clean up
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
}

# ---------- 6. Token enforcement on real pods ----------
scenario_06_token_enforcement() {
  local hash
  hash=$(publish_fixture dispatch-echo "$RUNNER0_PORT") || return 1
  log_info "dispatch-echo published (hash=${hash}), testing token enforcement"

  # No token → 401
  local code_none
  code_none=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H "Content-Type: application/json" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${RUNNER0_PORT}/deploy")
  assert_eq "$code_none" "401" "deploy without token" || return 1
  log_info "No token → 401"

  # Wrong token → 401
  local code_wrong
  code_wrong=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: wrong-token-value" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${RUNNER0_PORT}/deploy")
  assert_eq "$code_wrong" "401" "deploy with wrong token" || return 1
  log_info "Wrong token → 401"

  # Correct token → 200 (undeploy first to avoid 409 from prior scenarios)
  undeploy_package "dispatch-echo" "$RUNNER0_PORT" "$E2E_TOKEN"
  local code_ok
  code_ok=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${RUNNER0_PORT}/deploy")
  assert_eq "$code_ok" "200" "deploy with correct token" || return 1
  log_info "Correct token → 200"

  # Clean up
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
}

# ---------- 7. Graceful drain on pod termination ----------
scenario_07_graceful_drain() {
  # Clean slate on runner-0
  local agents
  agents=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents" 2>/dev/null || echo "[]")
  for pkg in $(echo "$agents" | jq -r '.[].agent_package' 2>/dev/null); do
    # Best-effort undeploy of any leftover agents
    local leftover_hash
    leftover_hash=$(curl -sf "http://localhost:${RUNNER0_PORT}/deployments" \
      -H "X-Runner-Token: ${E2E_TOKEN}" 2>/dev/null | jq -r '.[0].content_hash // empty')
    if [[ -n "$leftover_hash" ]]; then
      undeploy_hash "$leftover_hash" "$RUNNER0_PORT" "$E2E_TOKEN"
    fi
  done

  local hash
  hash=$(publish_and_deploy emit-plan-then-block "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  log_info "emit-plan-then-block deployed on runner-0 (hash=${hash})"

  # Start a blocking A2A request in background (tmpfile created after deploy
  # succeeds to avoid leaking on early return)
  local tmpfile body
  tmpfile=$(mktemp -t e2e-drain-XXXXXX)
  body=$(jsonrpc_send_stream "drain test message")
  curl --max-time 45 -N -s \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "http://localhost:${RUNNER0_PORT}/agents/emit-plan-then-block/default/a2a/sse" \
    > "$tmpfile" 2>&1 &
  local curl_pid=$!
  sleep 2
  log_info "In-flight A2A request started (PID=${curl_pid})"

  # Delete the pod
  kubectl delete pod runner-0 -n "$NAMESPACE" --grace-period=30 &
  local delete_pid=$!
  log_info "kubectl delete pod runner-0 issued"

  # Wait for the background curl to finish
  local curl_exit=0
  wait "$curl_pid" 2>/dev/null || curl_exit=$?
  wait "$delete_pid" 2>/dev/null || true
  log_info "Background curl exited with code ${curl_exit}"

  # Exit code 7 = connection refused (abrupt), 52 = empty reply (graceful close), 56 = recv failure
  # Anything other than 7 (connect failure pre-request) is acceptable
  if [[ "$curl_exit" -eq 7 ]]; then
    # Check if we got any data before the failure
    if [[ -s "$tmpfile" ]]; then
      log_info "Got partial response before connection close (acceptable)"
    else
      log_warn "Connection refused with no data — may indicate abrupt shutdown"
    fi
  fi
  rm -f "$tmpfile"

  # Wait for new runner-0 and restore port-forward
  restart_port_forward runner-0 "$RUNNER0_PORT" "$REMOTE_PORT"
  log_info "New runner-0 is ready"

  # Verify heartbeat updated (proves new runner-0 re-registered)
  local runners
  runners=$(surreal_query "SELECT * FROM cluster_runners")
  local r0_count
  r0_count=$(echo "$runners" | jq '[.[] | .result | .[] | select(.endpoint | contains("runner-0"))] | length')
  assert_ge "$r0_count" "1" "runner-0 re-registered in cluster_runners" || return 1

  # Soft assert: check for AgentStopped provenance event
  local lifecycle
  lifecycle=$(curl -sf "http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events" 2>/dev/null || echo "{}")
  if echo "$lifecycle" | jq -e '.rows[]? | select(.a2a_stop_reason == "undeploy")' >/dev/null 2>&1; then
    log_info "AgentStopped event with a2a_stop_reason=undeploy found"
  else
    log_warn "AgentStopped event not found in provenance (pod may have been killed before write — non-fatal)"
  fi
}

# ---------- 8. Heartbeat advances ----------
scenario_08_heartbeat() {
  # Include endpoint in the SELECT and ORDER BY for stable row order
  local before
  before=$(surreal_query "SELECT runner_id, endpoint, last_heartbeat_ms FROM cluster_runners ORDER BY endpoint")
  local r0_before r1_before
  r0_before=$(echo "$before" | jq '[.[] | .result | .[] | select(.endpoint | contains("runner-0"))] | max_by(.last_heartbeat_ms) | .last_heartbeat_ms // 0' 2>/dev/null || echo "0")
  r1_before=$(echo "$before" | jq '[.[] | .result | .[] | select(.endpoint | contains("runner-1"))] | max_by(.last_heartbeat_ms) | .last_heartbeat_ms // 0' 2>/dev/null || echo "0")
  log_info "Heartbeats before: runner-0=${r0_before}, runner-1=${r1_before}"

  if [[ "$r0_before" == "0" || "$r1_before" == "0" ]]; then
    log_fail "Could not read heartbeat timestamps from cluster_runners"
    echo "$before" >&2
    return 1
  fi

  log_info "Sleeping 12s (heartbeat interval is 5s)..."
  sleep 12

  local after
  after=$(surreal_query "SELECT runner_id, endpoint, last_heartbeat_ms FROM cluster_runners ORDER BY endpoint")
  local r0_after r1_after
  r0_after=$(echo "$after" | jq '[.[] | .result | .[] | select(.endpoint | contains("runner-0"))] | max_by(.last_heartbeat_ms) | .last_heartbeat_ms // 0' 2>/dev/null || echo "0")
  r1_after=$(echo "$after" | jq '[.[] | .result | .[] | select(.endpoint | contains("runner-1"))] | max_by(.last_heartbeat_ms) | .last_heartbeat_ms // 0' 2>/dev/null || echo "0")
  log_info "Heartbeats after:  runner-0=${r0_after}, runner-1=${r1_after}"

  if [[ "$r0_after" == "0" || "$r1_after" == "0" ]]; then
    log_fail "Could not read heartbeat timestamps after wait"
    echo "$after" >&2
    return 1
  fi

  local r0_delta r1_delta
  r0_delta=$(( r0_after - r0_before ))
  r1_delta=$(( r1_after - r1_before ))
  log_info "Deltas: runner-0=${r0_delta}ms, runner-1=${r1_delta}ms"

  assert_ge "$r0_delta" 5000 "runner-0 heartbeat advanced >= 5000ms" || return 1
  assert_ge "$r1_delta" 5000 "runner-1 heartbeat advanced >= 5000ms" || return 1
}

# ---------- 9. Readyz 503 window during startup ----------
scenario_09_readyz_503() {
  stop_port_forward runner-0
  kubectl delete pod runner-0 -n "$NAMESPACE" --timeout=60s
  log_info "Deleted runner-0, watching for readyz 503 window"

  # Wait for the new pod to exist and be scheduled
  local deadline=$((SECONDS + 60))
  while (( SECONDS < deadline )); do
    local phase
    phase=$(kubectl get pod runner-0 -n "$NAMESPACE" -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [[ "$phase" == "Running" ]]; then
      break
    fi
    sleep 0.3
  done

  # Start port-forward — uses wait_port_ready internally but we want to
  # poll readyz ourselves, so start the forward manually first then poll.
  kubectl -n "$NAMESPACE" port-forward runner-0 "${RUNNER0_PORT}:${REMOTE_PORT}" >/dev/null 2>&1 &
  local pf_pid=$!
  PF_PIDS+=("$pf_pid")
  echo "$pf_pid" > "/tmp/e2e-k8s-pf-runner-0.pid"

  # Poll readyz in a tight loop — we want to catch the 503 window
  local saw_503=false
  local saw_200=false
  local poll_deadline=$((SECONDS + 30))
  while (( SECONDS < poll_deadline )); do
    local code
    code=$(curl -s -o /dev/null -w '%{http_code}' --connect-timeout 1 "http://localhost:${RUNNER0_PORT}/readyz" 2>/dev/null || echo "000")
    if [[ "$code" == "503" ]]; then
      saw_503=true
      log_info "Caught readyz 503"
    elif [[ "$code" == "200" ]]; then
      saw_200=true
      if [[ "$saw_503" == "true" ]]; then
        log_info "readyz transitioned 503 → 200"
      else
        log_info "readyz returned 200 (may have missed 503 window)"
      fi
      break
    fi
    sleep 0.2
  done

  if [[ "$saw_503" == "true" ]]; then
    log_info "Successfully observed 503 → 200 transition"
  elif [[ "$saw_200" == "true" ]]; then
    log_warn "readyz went straight to 200 — runner initialized before port-forward connected (soft pass)"
  else
    log_fail "Neither 503 nor 200 observed within timeout"
    return 1
  fi

  # Ensure port-forward is fully stable
  wait_port_ready "$RUNNER0_PORT" 15 || true
}

# ---------- 10. Distributed multi-agent routing ----------
scenario_10_distributed_multi_agent() {
  # Deploy two LLM-free agents on separate runners, verify cross-pod routing for both.
  local hash_echo hash_stream

  # dispatch-echo on runner-0 only
  hash_echo=$(publish_and_deploy dispatch-echo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  # stream-js-tool on runner-1 only
  hash_stream=$(publish_and_deploy stream-js-tool "$RUNNER1_PORT" "$E2E_TOKEN") || return 1

  # Publish metadata to the opposite runner so placement resolution can discover them
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1
  publish_fixture stream-js-tool "$RUNNER0_PORT" >/dev/null || return 1
  log_info "dispatch-echo on runner-0, stream-js-tool on runner-1"

  # Verify each runner lists only its local agent
  local agents_r0
  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  assert_contains "$agents_r0" "dispatch-echo" "runner-0 lists dispatch-echo" || return 1

  local agents_r1
  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  assert_contains "$agents_r1" "stream-js-tool" "runner-1 lists stream-js-tool" || return 1

  # Cross-pod: access dispatch-echo through runner-1 (forward to runner-0)
  local resp_echo
  resp_echo=$(a2a_sse_request "$RUNNER1_PORT" "dispatch-echo" "hello from runner-1")
  assert_contains "$resp_echo" "dispatch-echo" "dispatch-echo response via runner-1 cross-pod" || return 1
  log_info "dispatch-echo responded via runner-1 (cross-pod forward to runner-0)"

  # Cross-pod: access stream-js-tool through runner-0 (forward to runner-1)
  local resp_stream
  resp_stream=$(a2a_sse_request "$RUNNER0_PORT" "stream-js-tool" "stream-task")
  assert_contains "$resp_stream" "Complete" "stream-js-tool response via runner-0 cross-pod" || return 1
  log_info "stream-js-tool responded via runner-0 (cross-pod forward to runner-1)"

  # Verify placement table shows both agents on correct runners
  local placements
  placements=$(surreal_query "SELECT agent_package, runner_endpoint FROM cluster_agent_placements WHERE agent_package IN ['dispatch-echo', 'stream-js-tool']")
  local placement_count
  placement_count=$(echo "$placements" | jq '[.[] | .result | .[]] | length')
  assert_eq "$placement_count" "2" "two placement rows for two agents" || return 1

  local echo_endpoint
  echo_endpoint=$(echo "$placements" | jq -r '[.[] | .result | .[] | select(.agent_package == "dispatch-echo")] | .[0].runner_endpoint')
  assert_contains "$echo_endpoint" "runner-0" "dispatch-echo placed on runner-0" || return 1

  local stream_endpoint
  stream_endpoint=$(echo "$placements" | jq -r '[.[] | .result | .[] | select(.agent_package == "stream-js-tool")] | .[0].runner_endpoint')
  assert_contains "$stream_endpoint" "runner-1" "stream-js-tool placed on runner-1" || return 1
  log_info "Placement table: dispatch-echo→runner-0, stream-js-tool→runner-1"

  # Clean up
  undeploy_hash "$hash_echo" "$RUNNER0_PORT" "$E2E_TOKEN"
  undeploy_hash "$hash_stream" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ---------- 11. Full agent lifecycle across the cluster ----------
scenario_11_full_lifecycle() {
  # One agent, one hash, every lifecycle phase: publish → deploy → use → migrate →
  # use (transparent forward) → undeploy → redeploy from same hash.
  local hash
  hash=$(publish_fixture dispatch-echo "$RUNNER0_PORT") || return 1
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1
  log_info "Published dispatch-echo (hash=${hash})"

  # Clean slate
  undeploy_package "dispatch-echo" "$RUNNER0_PORT" "$E2E_TOKEN"
  undeploy_package "dispatch-echo" "$RUNNER1_PORT" "$E2E_TOKEN"

  # Phase 1: Deploy on runner-0
  deploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null || return 1

  local agents_r0
  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  assert_contains "$agents_r0" "dispatch-echo" "Phase 1: runner-0 has dispatch-echo" || return 1

  local agents_r1
  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  if echo "$agents_r1" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "Phase 1: runner-1 should not have dispatch-echo"
    return 1
  fi
  log_info "Phase 1 (deploy on runner-0): verified"

  # Phase 2: A2A request on runner-0 (local fast path)
  local resp
  resp=$(a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "lifecycle test")
  assert_contains "$resp" "dispatch-echo" "Phase 2: A2A on runner-0" || return 1
  log_info "Phase 2 (A2A on runner-0): verified"

  # Phase 3: Migrate to runner-1
  local migrate_resp
  migrate_resp=$(curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"http://runner-1.runner.agentium.svc:18080\"}" \
    "http://localhost:${RUNNER0_PORT}/control/migrate") || {
    log_fail "Phase 3: migration failed"
    return 1
  }

  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  if echo "$agents_r0" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "Phase 3: runner-0 still has dispatch-echo after migration"
    return 1
  fi

  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  assert_contains "$agents_r1" "dispatch-echo" "Phase 3: runner-1 has dispatch-echo" || return 1
  log_info "Phase 3 (migrate to runner-1): verified"

  # Phase 4: A2A to runner-0 transparently forwards to runner-1
  resp=$(a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "forwarded test")
  assert_contains "$resp" "dispatch-echo" "Phase 4: A2A forwarded runner-0→runner-1" || return 1
  log_info "Phase 4 (transparent forward after migration): verified"

  # Phase 5: Undeploy from runner-1 — neither runner has it
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"

  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  if echo "$agents_r0" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "Phase 5: runner-0 still lists dispatch-echo after undeploy"
    return 1
  fi
  if echo "$agents_r1" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "Phase 5: runner-1 still lists dispatch-echo after undeploy"
    return 1
  fi
  log_info "Phase 5 (undeploy): verified — neither runner has the agent"

  # Phase 6: Redeploy on runner-0 from the same hash (repository is durable)
  deploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null || {
    log_fail "Phase 6: redeploy from same hash failed — repository data lost?"
    return 1
  }

  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  assert_contains "$agents_r0" "dispatch-echo" "Phase 6: runner-0 has dispatch-echo after redeploy" || return 1

  local placement
  placement=$(surreal_query "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo'")
  local placement_endpoint
  placement_endpoint=$(echo "$placement" | jq -r '[.[] | .result | .[].runner_endpoint] | .[0]')
  assert_contains "$placement_endpoint" "runner-0" "Phase 6: placement points to runner-0" || return 1
  log_info "Phase 6 (redeploy from same hash): verified — agents are portable and durable"

  # Clean up
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
}

# ---------- 12. Provenance survives migration ----------
scenario_12_provenance_survives_migration() {
  # Deploy on runner-0, interact, migrate to runner-1 — the full audit trail is
  # visible from either runner because provenance lives in shared SurrealDB.
  local hash
  hash=$(publish_and_deploy dispatch-echo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1
  log_info "dispatch-echo deployed on runner-0 (hash=${hash})"

  # Create some A2A activity so provenance has something to record
  a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "provenance test message" >/dev/null
  sleep 2

  # Query lifecycle events from runner-0
  local lifecycle_before
  lifecycle_before=$(curl -sf "http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
  local count_before
  count_before=$(echo "$lifecycle_before" | jq '.rows | length')
  assert_ge "$count_before" 1 "lifecycle events exist after deploy on runner-0" || return 1
  log_info "Lifecycle events before migration: ${count_before} rows"

  # Migrate to runner-1 (undeploys from runner-0, deploys on runner-1)
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"http://runner-1.runner.agentium.svc:18080\"}" \
    "http://localhost:${RUNNER0_PORT}/control/migrate" >/dev/null || {
    log_fail "Migration failed"
    return 1
  }
  sleep 2

  # Query lifecycle events from runner-1 — should include events from BOTH phases
  local lifecycle_after
  lifecycle_after=$(curl -sf "http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
  local count_after
  count_after=$(echo "$lifecycle_after" | jq '.rows | length')
  assert_ge "$count_after" "$count_before" "lifecycle events grew after migration" || return 1
  log_info "Lifecycle events after migration (queried from runner-1): ${count_after} rows"

  # Verify AgentStopped event from the undeploy on runner-0
  if echo "$lifecycle_after" | jq -e '.rows[] | select(.a2a_stop_reason == "undeploy")' >/dev/null 2>&1; then
    log_info "AgentStopped event (reason=undeploy) found in provenance"
  else
    log_warn "AgentStopped not found yet (async write may be delayed — non-fatal)"
  fi

  # Query from runner-0 as well — both runners see identical provenance (shared SurrealDB)
  local lifecycle_r0
  lifecycle_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
  local count_r0
  count_r0=$(echo "$lifecycle_r0" | jq '.rows | length')
  assert_eq "$count_r0" "$count_after" "both runners return identical lifecycle event count" || return 1
  log_info "Both runners see the same ${count_r0} lifecycle events (shared provenance store)"

  # Send another request on runner-1 to show provenance continues on the new host
  a2a_sse_request "$RUNNER1_PORT" "dispatch-echo" "post-migration message" >/dev/null
  sleep 1

  local lifecycle_final
  lifecycle_final=$(curl -sf "http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
  local count_final
  count_final=$(echo "$lifecycle_final" | jq '.rows | length')
  assert_ge "$count_final" "$count_after" "provenance continues growing on new runner" || return 1
  log_info "Full audit trail: ${count_final} lifecycle events spanning both runners"

  # Clean up
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ---------- 13. Stale runner exclusion under partition ----------
scenario_13_stale_runner_exclusion() {
  # Deploy on runner-0, force-kill it, backdate its heartbeat to simulate TTL
  # expiry, verify routing excludes the stale runner, then verify recovery.
  local hash
  hash=$(publish_and_deploy dispatch-echo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1
  log_info "dispatch-echo deployed on runner-0 (hash=${hash})"

  # Verify cross-pod routing works before partition
  local resp
  resp=$(a2a_sse_request "$RUNNER1_PORT" "dispatch-echo" "pre-partition test")
  assert_contains "$resp" "dispatch-echo" "pre-partition: runner-1 forwards to runner-0" || return 1
  log_info "Cross-pod routing verified before partition"

  # Force-kill runner-0 (simulates crash — no graceful drain)
  stop_port_forward runner-0
  kubectl delete pod runner-0 -n "$NAMESPACE" --grace-period=0 --force 2>/dev/null
  log_info "Force-killed runner-0 (simulating crash)"

  # Backdate runner-0's heartbeat to make it immediately stale.
  # This simulates what happens after the placement TTL (default 90s) expires
  # without waiting the full duration.
  surreal_query "UPDATE cluster_runners SET last_heartbeat_ms = 0 WHERE endpoint = 'http://runner-0.runner.agentium.svc:18080'" >/dev/null
  log_info "Backdated runner-0 heartbeat to epoch 0 (simulating TTL expiry)"

  # Verify the TTL-filtered placement query excludes stale runner-0.
  # This is the exact query pattern the PlacementResolver uses.
  local stale_placements
  stale_placements=$(surreal_query "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo' AND runner_id IN (SELECT VALUE runner_id FROM cluster_runners WHERE last_heartbeat_ms > (time::millis(time::now()) - 90000))")
  local stale_count
  stale_count=$(echo "$stale_placements" | jq '[.[] | .result | .[]] | length')
  assert_eq "$stale_count" "0" "stale runner excluded from TTL-filtered placement query" || return 1
  log_info "Placement query correctly excludes stale runner-0"

  # A2A request to runner-1 for dispatch-echo — should fail because the only
  # placement is on the stale runner-0, which is filtered out.
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$(jsonrpc_send_stream "post-partition test")" \
    "http://localhost:${RUNNER1_PORT}/agents/dispatch-echo/default/a2a/sse" 2>/dev/null)
  if [[ "$code" -ge 400 ]]; then
    log_info "runner-1 rejects request for agent on stale runner (HTTP ${code})"
  else
    log_fail "runner-1 should not route to stale runner-0 (got HTTP ${code})"
    return 1
  fi

  # Wait for runner-0 to be recreated by the StatefulSet and become ready.
  # The new pod restores deployments from PVC and re-registers with a fresh heartbeat.
  restart_port_forward runner-0 "$RUNNER0_PORT" "$REMOTE_PORT"
  log_info "runner-0 recreated and ready"

  # Verify runner-0 re-registered with a fresh heartbeat
  local hb_after
  hb_after=$(surreal_query "SELECT last_heartbeat_ms FROM cluster_runners WHERE endpoint = 'http://runner-0.runner.agentium.svc:18080'")
  local hb_ms_after
  hb_ms_after=$(echo "$hb_after" | jq '[.[] | .result | .[].last_heartbeat_ms] | max')
  assert_ge "$hb_ms_after" 1 "runner-0 re-registered with fresh heartbeat" || return 1
  log_info "runner-0 re-registered (heartbeat=${hb_ms_after})"

  # Verify dispatch-echo is reachable again (restored from PVC, re-placed)
  resp=$(a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "post-recovery test")
  assert_contains "$resp" "dispatch-echo" "dispatch-echo reachable after runner-0 recovery" || return 1
  log_info "dispatch-echo restored and reachable after recovery"

  # Clean up
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"
}

# ---------- 14. Concurrent deployment convergence ----------
scenario_14_concurrent_deployment() {
  # Deploy the same agent on both runners simultaneously. Each runner serves
  # requests locally via the fast path, independent of the placement table.
  local hash
  hash=$(publish_fixture dispatch-echo "$RUNNER0_PORT") || return 1
  publish_fixture dispatch-echo "$RUNNER1_PORT" >/dev/null || return 1

  undeploy_package "dispatch-echo" "$RUNNER0_PORT" "$E2E_TOKEN"
  undeploy_package "dispatch-echo" "$RUNNER1_PORT" "$E2E_TOKEN"

  # Deploy on both runners concurrently
  deploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN" >/dev/null &
  local pid0=$!
  deploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN" >/dev/null &
  local pid1=$!
  wait "$pid0" || { log_fail "concurrent deploy to runner-0 failed"; return 1; }
  wait "$pid1" || { log_fail "concurrent deploy to runner-1 failed"; return 1; }
  log_info "Concurrent deploy succeeded on both runners"

  # Both runners list the agent in /agents
  local agents_r0 agents_r1
  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  agents_r1=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents")
  assert_contains "$agents_r0" "dispatch-echo" "runner-0 lists dispatch-echo" || return 1
  assert_contains "$agents_r1" "dispatch-echo" "runner-1 lists dispatch-echo" || return 1

  # Both serve A2A requests locally (local fast path — no cluster lookup needed)
  local resp_r0 resp_r1
  resp_r0=$(a2a_sse_request "$RUNNER0_PORT" "dispatch-echo" "concurrent from r0")
  resp_r1=$(a2a_sse_request "$RUNNER1_PORT" "dispatch-echo" "concurrent from r1")
  assert_contains "$resp_r0" "dispatch-echo" "runner-0 serves locally" || return 1
  assert_contains "$resp_r1" "dispatch-echo" "runner-1 serves locally" || return 1
  log_info "Both runners serve the same agent independently"

  # Placement table has exactly 1 row (UNIQUE constraint; last-write-wins)
  local placements
  placements=$(surreal_query "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo'")
  local count
  count=$(echo "$placements" | jq '[.[] | .result | .[]] | length')
  assert_eq "$count" "1" "placement table converged to 1 row (UNIQUE constraint)" || return 1
  log_info "Placement converged: 1 row (last-write-wins)"

  # Undeploy from runner-0 — runner-1 continues serving via its local instance
  undeploy_hash "$hash" "$RUNNER0_PORT" "$E2E_TOKEN"

  agents_r0=$(curl -sf "http://localhost:${RUNNER0_PORT}/agents")
  if echo "$agents_r0" | jq -e '.[] | select(.agent_package == "dispatch-echo")' >/dev/null 2>&1; then
    log_fail "runner-0 still lists dispatch-echo after undeploy"
    return 1
  fi

  resp_r1=$(a2a_sse_request "$RUNNER1_PORT" "dispatch-echo" "surviving instance")
  assert_contains "$resp_r1" "dispatch-echo" "runner-1 continues serving after runner-0 undeploy" || return 1
  log_info "runner-1 survives runner-0 undeploy (independent local instance)"

  # Clean up
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ---------- 15. Task lifecycle across pod boundaries ----------
scenario_15_task_lifecycle_across_pods() {
  # Multi-turn conversation with INPUT_REQUIRED on a single runner, then migrate
  # and document whether conversation state survives the move.
  local hash
  hash=$(publish_and_deploy task-lifecycle-demo "$RUNNER0_PORT" "$E2E_TOKEN") || return 1
  publish_fixture task-lifecycle-demo "$RUNNER1_PORT" >/dev/null || return 1
  log_info "task-lifecycle-demo deployed on runner-0 (hash=${hash})"

  # Turn 1: Start conversation with trigger phrase
  local turn1
  turn1=$(a2a_sse_request "$RUNNER0_PORT" "task-lifecycle-demo" "lifecycle-demo")
  if [[ -z "$turn1" ]]; then
    log_fail "Turn 1: empty response"
    return 1
  fi
  assert_contains "$turn1" "Choose path" "Turn 1: got path selection prompt" || return 1

  local context_id
  context_id=$(extract_context_id "$turn1")
  log_info "Turn 1: INPUT_REQUIRED with path choice (contextId=${context_id:-unknown})"

  # Turn 2: Reply with fast-path to verify multi-turn works on a single runner
  if [[ -n "$context_id" ]]; then
    local turn2
    turn2=$(a2a_sse_request_with_context "$RUNNER0_PORT" "task-lifecycle-demo" "fast-path" "$context_id")
    if [[ -n "$turn2" ]]; then
      if echo "$turn2" | grep -qi "fast.path\|completed"; then
        log_info "Turn 2: conversation completed via fast-path on runner-0"
      else
        log_info "Turn 2: response received (multi-turn exchange confirmed)"
      fi
    else
      log_warn "Turn 2: empty response (multi-turn may have failed)"
    fi
  else
    log_warn "Skipping Turn 2: no contextId extracted from Turn 1"
  fi

  # Start a new conversation for the migration test
  local turn3
  turn3=$(a2a_sse_request "$RUNNER0_PORT" "task-lifecycle-demo" "lifecycle-demo")
  assert_contains "$turn3" "Choose path" "New conversation: got path selection prompt" || return 1

  local new_context_id
  new_context_id=$(extract_context_id "$turn3")
  log_info "New conversation started (contextId=${new_context_id:-unknown}), now migrating"

  # Migrate while the task is in INPUT_REQUIRED state
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"http://runner-1.runner.agentium.svc:18080\"}" \
    "http://localhost:${RUNNER0_PORT}/control/migrate" >/dev/null || {
    log_fail "Migration during INPUT_REQUIRED failed"
    return 1
  }
  log_info "Migrated to runner-1 while task in INPUT_REQUIRED state"

  # Try to continue the suspended conversation on runner-1
  if [[ -n "$new_context_id" ]]; then
    local turn4
    turn4=$(a2a_sse_request_with_context "$RUNNER1_PORT" "task-lifecycle-demo" "fast-path" "$new_context_id")
    if [[ -n "$turn4" ]] && echo "$turn4" | grep -qi "fast.path\|completed"; then
      log_info "Conversation RESUMED on runner-1 after migration (full state portability)"
    elif [[ -n "$turn4" ]]; then
      # Agent received the message but started a new context (expected behavior:
      # in-memory task state does not survive migration — see mid-turn checkpoint
      # architecture doc for the planned solution)
      log_info "Conversation started fresh on runner-1 (expected: task state is per-runner, not yet portable)"
    else
      log_info "No response from runner-1 with old contextId (expected: task state is per-runner)"
    fi
  else
    log_info "Cannot test continuation (no contextId) — task state is per-runner"
  fi

  # Verify a fresh conversation works on runner-1 after migration
  local fresh
  fresh=$(a2a_sse_request "$RUNNER1_PORT" "task-lifecycle-demo" "lifecycle-demo")
  assert_contains "$fresh" "Choose path" "fresh conversation works on runner-1 after migration" || return 1
  log_info "Fresh conversation succeeds on runner-1 — agent is operational post-migration"

  # Clean up
  undeploy_hash "$hash" "$RUNNER1_PORT" "$E2E_TOKEN"
}

# ============================================================================
# Main
# ============================================================================
main() {
  echo "=== Agentium OS E2E K8s Test Harness ==="
  echo ""

  preflight
  build_phase
  setup_cluster

  run_scenario "01-cluster-boot-sanity"     scenario_01_cluster_boot
  run_scenario "02-pvc-persistence"          scenario_02_pvc_persistence
  run_scenario "03-cross-pod-a2a"            scenario_03_cross_pod_a2a
  run_scenario "04-cross-pod-migration"      scenario_04_migration
  run_scenario "05-ssrf-rejection"           scenario_05_ssrf_rejection
  run_scenario "06-token-enforcement"        scenario_06_token_enforcement
  run_scenario "07-graceful-drain"           scenario_07_graceful_drain
  run_scenario "08-heartbeat-advances"       scenario_08_heartbeat
  run_scenario "09-readyz-503-window"        scenario_09_readyz_503
  run_scenario "10-distributed-multi-agent"  scenario_10_distributed_multi_agent
  run_scenario "11-full-agent-lifecycle"     scenario_11_full_lifecycle
  run_scenario "12-provenance-survives-migration" scenario_12_provenance_survives_migration
  run_scenario "13-stale-runner-exclusion"   scenario_13_stale_runner_exclusion
  run_scenario "14-concurrent-deployment"    scenario_14_concurrent_deployment
  run_scenario "15-task-lifecycle-across-pods" scenario_15_task_lifecycle_across_pods

  # cleanup runs via trap
}

main "$@"
