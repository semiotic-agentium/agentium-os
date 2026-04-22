#!/usr/bin/env bash
# Shared helpers for the E2E k8s test harness.
# Sourced by run.sh — not executed directly.

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
NAMESPACE="agentium"
CLUSTER_NAME="agentium"
RELEASE_NAME="agentium"                 # helm release; matches examples/k3d-values.yaml
IMAGE_NAME="agentium-runner"
IMAGE_TAG="demo"                        # aligns with examples/k3d-values.yaml
IMAGE_PULL_POLICY="Never"               # local-k3d-import strategy
SURREAL_USER="e2e"
SURREAL_PASS="e2e-test-pass"
E2E_TOKEN="e2e-token-${RANDOM}"         # also written into runner-token secret
RUNNER0_PORT=18081
RUNNER1_PORT=18082
REMOTE_PORT=18080
RUNNER_CONTAINER_PORT=18080             # chart value runner.service.headless.port
HELM_VALUES_FILE="deploy/helm/agentium-os/examples/k3d-values.yaml"
RUNNER_IMAGE_STRATEGY="${RUNNER_IMAGE_STRATEGY:-local-k3d-import}"

# Populated at runtime by resolve_chart_names (after helm install)
RUNNER_FULLNAME=""      # e.g. agentium-agentium-os-runner
SURREAL_FULLNAME=""     # e.g. agentium-agentium-os-surrealdb
RUNNER_API_SERVICE=""   # e.g. agentium-agentium-os-runner-api
RUNNER_POD_0=""
RUNNER_POD_1=""
SURREAL_POD_0=""
RUNNER_HEADLESS_DNS=""  # ${RUNNER_FULLNAME}.${NAMESPACE}.svc

# Populated at runtime
REPO_ROOT=""
BUILDER_BIN=""
PF_PIDS=()           # port-forward PIDs for cleanup
SCENARIO_RESULTS=()  # "PASS|FAIL name duration"
HAS_FAILURE=0
LOG_DIR=""

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
log_info()  { echo "  [INFO]  $*"; }
log_pass()  { echo "  [PASS]  $*"; }
log_fail()  { echo "  [FAIL]  $*"; HAS_FAILURE=1; }
log_warn()  { echo "  [WARN]  $*"; }
log_step()  { echo ""; echo "--- $* ---"; }

# ---------------------------------------------------------------------------
# Port-forward management
# ---------------------------------------------------------------------------
# start_port_forward <pod> <local_port> <remote_port>
start_port_forward() {
  local pod="$1" local_port="$2" remote_port="$3"
  kubectl -n "$NAMESPACE" port-forward "$pod" "${local_port}:${remote_port}" >/dev/null 2>&1 &
  local pid=$!
  PF_PIDS+=("$pid")
  echo "$pid" > "/tmp/e2e-k8s-pf-${pod}.pid"
  # Wait for TCP accept
  wait_port_ready "$local_port" 30
}

# stop_port_forward <pod>
stop_port_forward() {
  local pod="$1"
  local pidfile="/tmp/e2e-k8s-pf-${pod}.pid"
  if [[ -f "$pidfile" ]]; then
    local pid
    pid=$(cat "$pidfile")
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    rm -f "$pidfile"
  fi
}

# restart_port_forward <pod> <local_port> <remote_port>
restart_port_forward() {
  local pod="$1" local_port="$2" remote_port="$3"
  stop_port_forward "$pod"
  kubectl -n "$NAMESPACE" wait --for=condition=ready "pod/${pod}" --timeout=120s >/dev/null 2>&1
  start_port_forward "$pod" "$local_port" "$remote_port"
}

# wait_port_ready <port> <timeout_seconds>
wait_port_ready() {
  local port="$1" timeout="${2:-30}"
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:${port}/healthz" 2>/dev/null; then
      return 0
    fi
    sleep 0.3
  done
  log_warn "Port $port did not become ready within ${timeout}s"
  return 1
}

# kill_all_port_forwards
kill_all_port_forwards() {
  for pidfile in /tmp/e2e-k8s-pf-*.pid; do
    [[ -f "$pidfile" ]] || continue
    local pid
    pid=$(cat "$pidfile")
    kill "$pid" 2>/dev/null || true
    rm -f "$pidfile"
  done
  for pid in "${PF_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  PF_PIDS=()
}

# ---------------------------------------------------------------------------
# Fixture publishing & deployment
# ---------------------------------------------------------------------------

# Per-runner hash cache: HASH_CACHE_<port>_<fixture>=<hash>
# Avoids re-publishing the same fixture (version increments change the hash).
declare -A PUBLISH_CACHE

# publish_fixture <fixture_name> <local_port>
# Publishes a fixture agent to the runner's repository. Echoes the content hash.
# Caches the result per (fixture, port) pair so repeated calls return the same hash.
publish_fixture() {
  local fixture="$1" port="$2"
  local cache_key="${port}:${fixture}"
  if [[ -n "${PUBLISH_CACHE[$cache_key]:-}" ]]; then
    echo "${PUBLISH_CACHE[$cache_key]}"
    return 0
  fi
  local output
  # The repository POST endpoint is an operator-auth route; the builder
  # CLI sends X-Runner-Token when --runner-token is passed (or RUNNER_TOKEN
  # is in env).
  output=$("$BUILDER_BIN" publish \
    --agent-dir "${REPO_ROOT}/tests/fixtures/agents/${fixture}" \
    --repository-url "http://localhost:${port}/repository" \
    --runner-token "$E2E_TOKEN" 2>&1)
  local hash
  hash=$(echo "$output" | grep 'content_hash:' | awk '{print $2}' | tr -d '[:space:]')
  if [[ -z "$hash" ]]; then
    log_fail "publish_fixture ${fixture}: could not extract hash from builder output"
    echo "$output" >&2
    return 1
  fi
  PUBLISH_CACHE[$cache_key]="$hash"
  echo "$hash"
}

# undeploy_package <package_name> <local_port> <token>
# Undeploys any active deployment of the named agent package.
undeploy_package() {
  local pkg="$1" port="$2" token="$3"
  local agents
  agents=$(curl -sf "http://localhost:${port}/agents" 2>/dev/null || echo "[]")
  local hash
  hash=$(echo "$agents" | jq -r ".[] | select(.agent_package == \"${pkg}\") | .agent_card.content_hash // empty" 2>/dev/null | head -1)
  if [[ -n "$hash" ]]; then
    undeploy_hash "$hash" "$port" "$token"
  fi
}

# deploy_hash <hash> <local_port> <token>
deploy_hash() {
  local hash="$1" port="$2" token="$3"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${token}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${port}/deploy"
}

# undeploy_hash <hash> <local_port> <token>
undeploy_hash() {
  local hash="$1" port="$2" token="$3"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${token}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${port}/undeploy" 2>/dev/null || true
}

# publish_and_deploy <fixture_name> <local_port> <token>
# Undeploys any existing deployment of the same package first, then publishes and deploys.
# Echoes the content hash.
publish_and_deploy() {
  local fixture="$1" port="$2" token="$3"
  undeploy_package "$fixture" "$port" "$token"
  local hash
  hash=$(publish_fixture "$fixture" "$port") || return 1
  deploy_hash "$hash" "$port" "$token" >/dev/null || {
    log_fail "deploy failed for ${fixture} (hash=${hash})"
    return 1
  }
  echo "$hash"
}

# ---------------------------------------------------------------------------
# SurrealDB queries
# ---------------------------------------------------------------------------

# surreal_query <sql> [namespace] [database]
# Executes a SurrealQL statement via kubectl exec on the SurrealDB pod.
# Defaults: namespace=cluster, database=registry.
# Requires resolve_chart_names to have populated $SURREAL_POD_0.
surreal_query() {
  local sql="$1"
  local ns="${2:-cluster}"
  local db="${3:-registry}"
  if [[ -z "$SURREAL_POD_0" ]]; then
    # Soft fail: callers in cleanup paths use `|| true` and expect a
    # non-zero return, not a hard shell exit. ${VAR:?} would kill the
    # whole script right through any protective `|| true`.
    log_warn "surreal_query: SURREAL_POD_0 not set — call resolve_chart_names first"
    return 1
  fi
  local raw
  raw=$(echo "$sql" | kubectl exec -n "$NAMESPACE" "$SURREAL_POD_0" -c surrealdb -i -- \
    /surreal sql \
    --endpoint http://localhost:8000 \
    --username "$SURREAL_USER" \
    --password "$SURREAL_PASS" \
    --namespace "$ns" \
    --database "$db" \
    --json 2>/dev/null)
  # SurrealDB v3 outputs [[...rows...]], normalize to v2-like [{"result": [...rows...]}]
  # so existing jq expressions using `.[] | .result | .[]` continue to work.
  echo "$raw" | jq '[{result: .[0] // []}]' 2>/dev/null || echo '[{"result":[]}]'
}

# ---------------------------------------------------------------------------
# A2A helpers
# ---------------------------------------------------------------------------

# jsonrpc_send_stream <text>
# Echoes a JSON-RPC 2.0 message.sendStream request body.
jsonrpc_send_stream() {
  local text="$1"
  local msg_id corr_id millis
  millis=$(python3 -c 'import time; print(int(time.time()*1000))')
  msg_id="e2e-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  cat <<JSONEOF
{"jsonrpc":"2.0","id":"${corr_id}","method":"message.sendStream","params":{"message":{"messageId":"${msg_id}","role":"user","parts":[{"kind":"text","text":"${text}"}]}}}
JSONEOF
}

# a2a_sse_request <local_port> <agent_package> <text>
# Sends an A2A SSE request. Echoes the response body.
a2a_sse_request() {
  local port="$1" pkg="$2" text="$3"
  local body
  body=$(jsonrpc_send_stream "$text")
  curl -sf --max-time 30 -N \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "http://localhost:${port}/agents/${pkg}/default/a2a" 2>/dev/null
}

# jsonrpc_send_stream_with_context <text> <context_id>
# Echoes a JSON-RPC 2.0 message.sendStream request body with a contextId for multi-turn.
jsonrpc_send_stream_with_context() {
  local text="$1" context_id="$2"
  local msg_id corr_id millis
  millis=$(python3 -c 'import time; print(int(time.time()*1000))')
  msg_id="e2e-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  cat <<JSONEOF
{"jsonrpc":"2.0","id":"${corr_id}","method":"message.sendStream","params":{"message":{"messageId":"${msg_id}","role":"user","parts":[{"kind":"text","text":"${text}"}],"contextId":"${context_id}"}}}
JSONEOF
}

# a2a_sse_request_with_context <local_port> <agent_package> <text> <context_id>
# Sends an A2A SSE request with a contextId for multi-turn continuation. Echoes the response body.
a2a_sse_request_with_context() {
  local port="$1" pkg="$2" text="$3" context_id="$4"
  local body
  body=$(jsonrpc_send_stream_with_context "$text" "$context_id")
  curl -sf --max-time 30 -N \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "http://localhost:${port}/agents/${pkg}/default/a2a" 2>/dev/null
}

# extract_context_id <sse_response>
# Extracts the first contextId value found in SSE data events.
extract_context_id() {
  local sse_response="$1"
  echo "$sse_response" | grep -o '"contextId":"[^"]*"' | head -1 | sed 's/"contextId":"//' | sed 's/"$//'
}

# ---------------------------------------------------------------------------
# Assertion helpers
# ---------------------------------------------------------------------------

# assert_eq <actual> <expected> <message>
assert_eq() {
  local actual="$1" expected="$2" msg="$3"
  if [[ "$actual" == "$expected" ]]; then
    return 0
  fi
  log_fail "$msg: expected '$expected', got '$actual'"
  return 1
}

# assert_ne <actual> <unexpected> <message>
assert_ne() {
  local actual="$1" unexpected="$2" msg="$3"
  if [[ "$actual" != "$unexpected" ]]; then
    return 0
  fi
  log_fail "$msg: got unexpected value '$actual'"
  return 1
}

# assert_contains <haystack> <needle> <message>
assert_contains() {
  local haystack="$1" needle="$2" msg="$3"
  if echo "$haystack" | grep -qF "$needle"; then
    return 0
  fi
  log_fail "$msg: '$needle' not found in output"
  return 1
}

# assert_http_code <expected_code> <curl_args...>
# Runs curl and checks the HTTP status code.
assert_http_code() {
  local expected="$1"; shift
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' "$@" 2>/dev/null)
  if [[ "$code" == "$expected" ]]; then
    return 0
  fi
  log_fail "HTTP status: expected ${expected}, got ${code} for: $*"
  return 1
}

# assert_ge <actual> <minimum> <message>
# Integer greater-than-or-equal assertion.
assert_ge() {
  local actual="$1" minimum="$2" msg="$3"
  if (( actual >= minimum )); then
    return 0
  fi
  log_fail "$msg: expected >= ${minimum}, got ${actual}"
  return 1
}

# ---------------------------------------------------------------------------
# Log capture
# ---------------------------------------------------------------------------

# dump_logs [directory]
# Dumps pod logs and SurrealDB state to the given directory.
dump_logs() {
  local dir="${1:-./e2e-k8s-logs/$(date +%Y%m%d-%H%M%S)}"
  mkdir -p "$dir"
  log_info "Dumping logs to ${dir}/"
  local pods=()
  if [[ -n "$RUNNER_POD_0" && -n "$RUNNER_POD_1" && -n "$SURREAL_POD_0" ]]; then
    pods=("$RUNNER_POD_0" "$RUNNER_POD_1" "$SURREAL_POD_0")
  else
    # resolve_chart_names did not run (bringup failed before install). Fall back
    # to label-based discovery so we still capture whatever pods exist.
    while read -r p; do pods+=("$p"); done < <(
      kubectl -n "$NAMESPACE" get pods \
        -l app.kubernetes.io/instance="$RELEASE_NAME" \
        -o jsonpath='{.items[*].metadata.name}' 2>/dev/null | tr ' ' '\n'
    )
    if (( ${#pods[@]} == 0 )); then
      log_warn "No chart pods found for release '${RELEASE_NAME}' in namespace '${NAMESPACE}' — bringup likely failed before install"
    fi
  fi
  for pod in "${pods[@]}"; do
    [[ -z "$pod" ]] && continue
    kubectl logs -n "$NAMESPACE" "$pod" --all-containers > "${dir}/${pod}.log" 2>&1 || true
    kubectl logs -n "$NAMESPACE" "$pod" --all-containers --previous > "${dir}/${pod}-previous.log" 2>&1 || true
  done
  # Dump cluster tables
  surreal_query "SELECT * FROM cluster_runners" > "${dir}/cluster_runners.json" 2>&1 || true
  surreal_query "SELECT * FROM cluster_agent_placements" > "${dir}/cluster_agent_placements.json" 2>&1 || true
  log_info "Log dump complete: ${dir}/"
}

# ---------------------------------------------------------------------------
# LLM key detection
# ---------------------------------------------------------------------------

# has_llm_keys — returns 0 if the cluster will actually be able to resolve
# the LLM credential scenario 10 needs (OPENROUTER_API_KEY). The supported
# install mounts `${REPO_ROOT}/fnox.toml` as the `fnox-config` ConfigMap,
# so the signal is: does that file carry a `default = …` under the
# `[secrets.OPENROUTER_API_KEY]` section? An unrelated default (e.g.
# `[secrets.CLICKUP_API_KEY]`) must not trip this — scenario 10 would
# then run against pods that still can't resolve OPENROUTER_API_KEY.
has_llm_keys() {
  local repo_root
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  [[ -f "${repo_root}/fnox.toml" ]] || return 1
  awk '
    /^\[secrets\.OPENROUTER_API_KEY\][[:space:]]*$/ { in_section = 1; next }
    /^\[/                                           { in_section = 0 }
    in_section && /^default[[:space:]]*=/           { found = 1; exit }
    END { exit !found }
  ' "${repo_root}/fnox.toml"
}

# ---------------------------------------------------------------------------
# Package bringup (Helm)
#
# Shared by scripts/verify-k8s-pilot-package.sh and scripts/e2e-k8s/run.sh.
# Both scripts install the chart the same way; only the set of verifications
# that run afterwards differs.
# ---------------------------------------------------------------------------

# ensure_runner_image_available — make ${IMAGE_NAME}:${IMAGE_TAG} reachable
# from the cluster. Strategy selected by $RUNNER_IMAGE_STRATEGY.
ensure_runner_image_available() {
  case "$RUNNER_IMAGE_STRATEGY" in
    local-k3d-import)
      log_step "Importing runner image into k3d (${IMAGE_NAME}:${IMAGE_TAG})"
      local import_tar import_name="${IMAGE_NAME}:${IMAGE_TAG}"
      import_tar="$(mktemp -t agentium-image-XXXXXX).tar"
      if [[ "$IMAGE_NAME" != */* ]]; then
        # Bare name: kubelet normalizes pod refs like `agentium-runner:demo`
        # to `docker.io/library/agentium-runner:demo`. Save under the
        # normalized name so k3d/containerd match the pod ref. Qualified
        # names (e.g. `ghcr.io/acme/agentium-runner`) already match the
        # pod ref verbatim and must not be retagged.
        docker tag "${IMAGE_NAME}:${IMAGE_TAG}" \
          "docker.io/library/${IMAGE_NAME}:${IMAGE_TAG}" 2>/dev/null || true
        import_name="docker.io/library/${IMAGE_NAME}:${IMAGE_TAG}"
      fi
      docker save -o "$import_tar" "$import_name"
      k3d image import "$import_tar" -c "$CLUSTER_NAME"
      rm -f "$import_tar"
      IMAGE_PULL_POLICY="Never"
      ;;
    registry)
      log_fail "RUNNER_IMAGE_STRATEGY=registry is not wired in this repo yet."
      echo "  Build and push the runner image to a cluster-reachable registry,"
      echo "  then pass --image-repository / --image-tag to the caller and use"
      echo "  deploy/helm/agentium-os/examples/design-partner-values.yaml."
      return 1
      ;;
    *)
      log_fail "Unknown RUNNER_IMAGE_STRATEGY: '$RUNNER_IMAGE_STRATEGY' (expected local-k3d-import|registry)"
      return 1
      ;;
  esac
}

# create_pilot_objects — create the three objects the chart requires:
# `surrealdb-credentials` secret, `runner-token` secret, `fnox-config`
# ConfigMap. The runner does not read LLM credentials from env vars (see
# docs/k8s-pilot-operator-guide.md "Create secrets and config"), so
# .env / fnox-secrets shortcuts stay in the demo script, not here.
create_pilot_objects() {
  log_step "Creating namespace and package objects"
  kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

  kubectl -n "$NAMESPACE" create secret generic surrealdb-credentials \
    --from-literal=username="$SURREAL_USER" \
    --from-literal=password="$SURREAL_PASS" \
    --dry-run=client -o yaml | kubectl apply -f -

  kubectl -n "$NAMESPACE" create secret generic runner-token \
    --from-literal=token="$E2E_TOKEN" \
    --dry-run=client -o yaml | kubectl apply -f -

  local fnox_src="${REPO_ROOT}/fnox.toml"
  if [[ -f "$fnox_src" ]]; then
    kubectl -n "$NAMESPACE" create configmap fnox-config \
      --from-file=fnox.toml="$fnox_src" \
      --dry-run=client -o yaml | kubectl apply -f -
    log_info "fnox-config ConfigMap sourced from ${fnox_src}"
  else
    # Placeholder valid TOML; dispatch-echo does not make LLM calls, so the
    # runner boots and serves /healthz + /readyz without a populated fnox.
    local placeholder
    placeholder="$(mktemp -t fnox-placeholder-XXXXXX.toml)"
    cat >"$placeholder" <<'TOML'
# Placeholder fnox config for package validation when no repo-root fnox.toml
# is available. Scenarios that require LLM credentials need a real fnox.toml
# in the repo root before running.
[secrets.OPENROUTER_API_KEY]
if_missing = "ignore"
TOML
    kubectl -n "$NAMESPACE" create configmap fnox-config \
      --from-file=fnox.toml="$placeholder" \
      --dry-run=client -o yaml | kubectl apply -f -
    rm -f "$placeholder"
    log_warn "No repo-root fnox.toml found — fnox-config is a placeholder; LLM-dependent scenarios will fail"
  fi
}

# rustlog_override <RUST_LOG-value>
#
# Echo a `--set-string` pair that safely passes <RUST_LOG-value> to the
# chart's runner.logging.rustLog knob. Helm splits on commas inside --set
# values, so they must be backslash-escaped; this helper hides that.
rustlog_override() {
  local value="${1//,/\\,}"
  echo "--set-string"
  echo "runner.logging.rustLog=${value}"
}

# install_pilot_chart [extra helm args]...
#
# `helm upgrade --install` using the k3d values file, overriding image
# fields. Image-strategy choice is resolved beforehand by
# ensure_runner_image_available.
install_pilot_chart() {
  log_step "Installing Helm chart ${RELEASE_NAME} (namespace ${NAMESPACE})"
  helm upgrade --install "$RELEASE_NAME" \
    "${REPO_ROOT}/deploy/helm/agentium-os/" \
    --namespace "$NAMESPACE" \
    -f "${REPO_ROOT}/${HELM_VALUES_FILE}" \
    --set-string "runner.image.repository=${IMAGE_NAME}" \
    --set-string "runner.image.tag=${IMAGE_TAG}" \
    --set-string "runner.image.pullPolicy=${IMAGE_PULL_POLICY}" \
    "$@"
}

# resolve_chart_names — populate runtime name variables from the installed
# release. Label-based lookup stays robust under fullnameOverride /
# nameOverride. Pod names follow the StatefulSet ordinal convention
# (`<statefulset>-<N>`).
resolve_chart_names() {
  local sts_json
  sts_json="$(kubectl -n "$NAMESPACE" get statefulset \
    -l "app.kubernetes.io/instance=${RELEASE_NAME}" -o json)"
  RUNNER_FULLNAME="$(echo "$sts_json" \
    | jq -r '.items[] | select(.metadata.labels["app.kubernetes.io/component"] == "runner") | .metadata.name')"
  SURREAL_FULLNAME="$(echo "$sts_json" \
    | jq -r '.items[] | select(.metadata.labels["app.kubernetes.io/component"] == "surrealdb") | .metadata.name')"
  if [[ -z "$RUNNER_FULLNAME" || -z "$SURREAL_FULLNAME" ]]; then
    log_fail "resolve_chart_names: could not find chart-rendered StatefulSets (runner='${RUNNER_FULLNAME}', surrealdb='${SURREAL_FULLNAME}')"
    return 1
  fi
  RUNNER_POD_0="${RUNNER_FULLNAME}-0"
  RUNNER_POD_1="${RUNNER_FULLNAME}-1"
  SURREAL_POD_0="${SURREAL_FULLNAME}-0"
  RUNNER_HEADLESS_DNS="${RUNNER_FULLNAME}.${NAMESPACE}.svc"
  RUNNER_API_SERVICE="${RUNNER_FULLNAME}-api"
  log_info "runner StatefulSet: ${RUNNER_FULLNAME} (pods ${RUNNER_POD_0}, ${RUNNER_POD_1})"
  log_info "surrealdb StatefulSet: ${SURREAL_FULLNAME} (pod ${SURREAL_POD_0})"
}

# runner_endpoint <0|1> — in-cluster HTTP endpoint for the Nth runner pod,
# in the shape the runner expects for cross-pod A2A (target_runner_endpoint,
# cluster_runners).
runner_endpoint() {
  if [[ -z "$RUNNER_FULLNAME" || -z "$RUNNER_HEADLESS_DNS" ]]; then
    log_warn "runner_endpoint: chart names not resolved yet — call resolve_chart_names first"
    return 1
  fi
  local index="$1"
  local pod="${RUNNER_FULLNAME}-${index}"
  echo "http://${pod}.${RUNNER_HEADLESS_DNS}:${RUNNER_CONTAINER_PORT}"
}

# wait_for_runner_readyz — wait until the runner StatefulSet rollout
# completes. The chart's readiness probe gates Pod Ready on /readyz 200,
# so rollout completion == /readyz green across all replicas.
wait_for_runner_readyz() {
  log_step "Waiting for runner StatefulSet rollout"
  kubectl -n "$NAMESPACE" rollout status \
    "statefulset/${RUNNER_FULLNAME}" --timeout=180s
  kubectl -n "$NAMESPACE" get pods -o wide
}

# ---------------------------------------------------------------------------
# Scenario runner
# ---------------------------------------------------------------------------

# run_scenario <name> <function>
# Executes a scenario function, captures duration and pass/fail.
run_scenario() {
  local name="$1" func="$2"
  log_step "$name"
  local start=$SECONDS
  local status="PASS"
  if ! "$func"; then
    status="FAIL"
    HAS_FAILURE=1
  fi
  local duration=$(( SECONDS - start ))
  SCENARIO_RESULTS+=("${status} ${name} ${duration}s")
  if [[ "$status" == "PASS" ]]; then
    log_pass "$name (${duration}s)"
  else
    log_fail "$name (${duration}s)"
  fi
}

# print_summary
print_summary() {
  echo ""
  echo "=== E2E K8s Test Summary ==="
  local passed=0 failed=0
  for result in "${SCENARIO_RESULTS[@]}"; do
    local st name dur
    st=$(echo "$result" | awk '{print $1}')
    name=$(echo "$result" | awk '{$1=""; $NF=""; print}' | xargs)
    dur=$(echo "$result" | awk '{print $NF}')
    printf "  [%-4s] %-40s %s\n" "$st" "$name" "$dur"
    # Use `$((...))` assignment, not `((++))` — the latter returns exit 1
    # when the pre-increment value is 0, which `set -e` treats as failure
    # and kills print_summary mid-loop on the first PASS.
    if [[ "$st" == "PASS" ]]; then passed=$((passed + 1)); else failed=$((failed + 1)); fi
  done
  echo ""
  echo "${passed}/$((passed + failed)) passed, ${failed}/$((passed + failed)) failed"
  if (( failed > 0 )) && [[ -n "$LOG_DIR" ]]; then
    echo "Logs: ${LOG_DIR}/"
  fi
}
