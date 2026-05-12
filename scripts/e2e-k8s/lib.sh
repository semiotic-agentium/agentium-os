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

# k3d-managed local registry contract (see deploy/k3d/cluster.yaml).
# The cluster pulls images from k3d-agentium-registry:5000; the host
# pushes to localhost:5400. ensure_runner_image_available wires both.
REGISTRY_NAME="agentium-registry"
REGISTRY_CONTAINER_HOST="k3d-${REGISTRY_NAME}"
REGISTRY_CONTAINER_PORT="5000"
REGISTRY_HOST_PORT="5400"

# Image reference passed to `helm install --set runner.image.repository=`.
# Both image strategies overwrite this in ensure_runner_image_available
# before install_pilot_chart runs; the source-time default exists so the
# variable is always defined for callers that introspect it.
IMAGE_REPOSITORY_FOR_INSTALL="$IMAGE_NAME"

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
# log_warn writes to stderr so callers whose stdout is reserved for a
# structured payload (e.g. surreal_query piped to a JSON file) can still
# emit warnings without corrupting the payload.
log_warn()  { echo "  [WARN]  $*" >&2; }
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

# Canonical SurrealDB namespaces defined by the agent-platform codebase.
# Sources (as of issue #388):
#   cluster    — baml-agent-runner src/main.rs (cluster routing, placements)
#   provenance — baml-rt-provenance src/surreal_store/schema.rs
#   config     — baml-rt-config src/store.rs
#   baml       — baml-rt-repository + baml-agent-runner deployment_state
# See deploy/helm/agentium-os/README.md#surrealdb-namespaces for the full
# NS/DB table. Update both this list and the README when a crate adds a
# new namespace.
SURREAL_KNOWN_NAMESPACES=(cluster provenance config baml)

# surreal_query <sql> [namespace] [database]
# Executes a SurrealQL statement via kubectl exec on the SurrealDB pod.
# Defaults: namespace=cluster, database=registry.
# Requires resolve_chart_names to have populated $SURREAL_POD_0.
#
# Output contract (stable, machine-readable in artifact dumps):
#   On success:
#     [{"result": [...rows...], "_query_status": "ok"}]
#   On failure (kubectl exec non-zero, non-JSON output, or unknown NS):
#     [{"result": [], "_query_status": "failed", "_error": "..."}]
#     and the function returns non-zero.
#
# The `_query_status` field disambiguates "table is empty" from "the
# query never reached SurrealDB" in CI artifacts. The outer-array shape
# is preserved so existing jq expressions (`.[] | .result | .[]`) keep
# working unchanged. Stderr from the helper itself stays on stderr — the
# stdout stream is reserved for the structured payload so callers can
# safely redirect it to a file.
#
# Namespace pre-check: SurrealDB v3 returns the cryptic "Couldn't write
# to a read only transaction" error when /sql is given a namespace that
# doesn't exist (issue #388). We guard against typos by checking $ns
# against SURREAL_KNOWN_NAMESPACES before forwarding the query.
surreal_query() {
  local sql="$1"
  local ns="${2:-cluster}"
  local db="${3:-registry}"
  local ns_known=0 known_ns
  for known_ns in "${SURREAL_KNOWN_NAMESPACES[@]}"; do
    if [[ "$known_ns" == "$ns" ]]; then
      ns_known=1
      break
    fi
  done
  if (( ns_known == 0 )); then
    local known_list
    known_list="$(IFS=,; echo "${SURREAL_KNOWN_NAMESPACES[*]}")"
    jq -nc --arg msg "unknown namespace: '${ns}'; known: ${known_list}. See deploy/helm/agentium-os/README.md#surrealdb-namespaces." \
      '[{result:[], _query_status:"failed", _error:$msg}]'
    return 1
  fi
  if [[ -z "$SURREAL_POD_0" ]]; then
    # Soft fail: callers in cleanup paths use `|| true` and expect a
    # non-zero return, not a hard shell exit. ${VAR:?} would kill the
    # whole script right through any protective `|| true`.
    log_warn "surreal_query: SURREAL_POD_0 not set — call resolve_chart_names first"
    jq -nc '[{result:[], _query_status:"failed", _error:"SURREAL_POD_0 not set"}]'
    return 1
  fi
  local stderr_file raw exec_status stderr_text
  stderr_file=$(mktemp)
  raw=$(echo "$sql" | kubectl exec -n "$NAMESPACE" "$SURREAL_POD_0" -c surrealdb -i -- \
    /surreal sql \
    --endpoint http://localhost:8000 \
    --username "$SURREAL_USER" \
    --password "$SURREAL_PASS" \
    --namespace "$ns" \
    --database "$db" \
    --json 2>"$stderr_file")
  exec_status=$?
  stderr_text=$(cat "$stderr_file")
  rm -f "$stderr_file"
  if (( exec_status != 0 )); then
    local err_msg="${stderr_text:-kubectl exec failed with status ${exec_status}}"
    jq -nc --arg msg "$err_msg" \
      '[{result:[], _query_status:"failed", _error:$msg}]'
    return 1
  fi
  # SurrealDB v3 outputs [[...rows...]]; normalize to v2-like
  # [{"result": [...rows...]}] so existing `.[] | .result | .[]` jq
  # paths keep working.
  if echo "$raw" | jq -c '[{result: (.[0] // []), _query_status: "ok"}]' 2>/dev/null; then
    return 0
  fi
  jq -nc --arg msg "$raw" \
    '[{result:[], _query_status:"failed", _error:("invalid JSON from surreal: " + $msg)}]'
  return 1
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
# Dumps pod logs, cluster diagnostics, and SurrealDB state into <directory>.
#
# In addition to per-pod logs and cluster_runners / cluster_agent_placements,
# captures the cluster-layer state that pod logs do not surface — pod
# descriptions (pull failures, OOM, eviction reasons), namespaced events
# (NetworkPolicy drops, scheduling failures), wide pod listing across all
# namespaces, and the chart-rendered StatefulSet/Service state.
# ConfigMap and Secret *data* are deliberately not captured: this
# codebase mounts fnox.toml (LLM API keys) into a ConfigMap, so a `-o
# yaml` dump would leak the keys into the artifact. Only ConfigMap and
# Secret names are recorded.
#
# After the initial captures, sleeps briefly and re-captures the tail of
# the runner pod logs into <pod>-tail.log so log lines emitted between the
# original dump and pod teardown are preserved (failures often produce
# their most diagnostic lines in this window).
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

  if (( ${#pods[@]} > 0 )); then
    kubectl describe pod -n "$NAMESPACE" "${pods[@]}" \
      > "${dir}/describe-pods.txt" 2>&1 || true
  fi
  kubectl get events -n "$NAMESPACE" --sort-by=.lastTimestamp \
    > "${dir}/events.txt" 2>&1 || true
  kubectl get pods -A -o wide \
    > "${dir}/all-pods.txt" 2>&1 || true
  kubectl get statefulset,svc -n "$NAMESPACE" -o yaml \
    > "${dir}/cluster-state.yaml" 2>&1 || true
  # ConfigMap and Secret data are intentionally excluded: this codebase
  # mounts fnox.toml (LLM API keys) into a ConfigMap, so `-o yaml` would
  # leak the keys into the artifact. Names only.
  kubectl get configmap -n "$NAMESPACE" -o name \
    > "${dir}/configmaps-list.txt" 2>&1 || true
  kubectl get secret -n "$NAMESPACE" -o name \
    > "${dir}/secrets-list.txt" 2>&1 || true

  surreal_query "SELECT * FROM cluster_runners" \
    > "${dir}/cluster_runners.json" || true
  surreal_query "SELECT * FROM cluster_agent_placements" \
    > "${dir}/cluster_agent_placements.json" || true

  # Settle, then re-tail: pod logs above are a single snapshot, and the
  # diagnostic 5xx body / panic typically lands in the seconds between
  # the snapshot and pod teardown.
  sleep 2
  for pod in "$RUNNER_POD_0" "$RUNNER_POD_1"; do
    [[ -z "$pod" ]] && continue
    kubectl logs -n "$NAMESPACE" "$pod" --all-containers --tail=400 \
      > "${dir}/${pod}-tail.log" 2>&1 || true
  done

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

# values_file_for_strategy <strategy>
#
# Echo the path (relative to repo root) of the Helm values file that
# matches the given image strategy. Returns non-zero on unknown input so
# callers can surface their own error message.
values_file_for_strategy() {
  case "$1" in
    local-k3d-import) echo "deploy/helm/agentium-os/examples/k3d-values.yaml" ;;
    registry)         echo "deploy/helm/agentium-os/examples/k3d-registry-values.yaml" ;;
    *) return 1 ;;
  esac
}

# ensure_k3d_registry — start (or reuse) a plain `registry:2` container
# named ${REGISTRY_CONTAINER_HOST} that the cluster will pull from.
#
# Used in place of k3d's `registries.create`, which is not portable across
# Docker and Podman (it tries to attach the registry to a `bridge` network
# that does not exist under rootful Podman). The container persists across
# k3d cluster runs; cleanup is via `docker rm -f ${REGISTRY_CONTAINER_HOST}`.
ensure_k3d_registry() {
  local state
  state="$(docker inspect "${REGISTRY_CONTAINER_HOST}" --format '{{.State.Status}}' 2>/dev/null || true)"
  if [[ -z "$state" ]]; then
    log_step "Starting registry '${REGISTRY_CONTAINER_HOST}' on host port ${REGISTRY_HOST_PORT}"
    docker run -d \
      --name "${REGISTRY_CONTAINER_HOST}" \
      --restart=unless-stopped \
      -p "${REGISTRY_HOST_PORT}:${REGISTRY_CONTAINER_PORT}" \
      docker.io/library/registry:2 >/dev/null
  elif [[ "$state" != "running" ]]; then
    log_info "Restarting registry '${REGISTRY_CONTAINER_HOST}'"
    docker start "${REGISTRY_CONTAINER_HOST}" >/dev/null
  else
    log_info "Reusing running registry '${REGISTRY_CONTAINER_HOST}'"
  fi
}

# connect_registry_to_cluster — attach ${REGISTRY_CONTAINER_HOST} to the
# k3d cluster's docker network so containerd inside the k3s nodes can
# resolve the registry hostname. The mirror config in
# deploy/k3d/cluster.yaml points containerd at this in-cluster name.
#
# Idempotent: skipping when already connected.
connect_registry_to_cluster() {
  local network="k3d-${CLUSTER_NAME}"
  # Use a pure-bash substring match instead of `... | grep -q`: under
  # `pipefail`, grep matching early triggers SIGPIPE on the upstream
  # producer and the whole pipeline returns 141, which would silently
  # take the "not attached" branch on a re-run with --keep-cluster.
  local containers
  containers="$(docker network inspect "$network" \
      --format '{{range .Containers}} {{.Name}} {{end}}' 2>/dev/null || true)"
  if [[ "$containers" == *" ${REGISTRY_CONTAINER_HOST} "* ]]; then
    log_info "Registry already attached to network '${network}'"
    return 0
  fi
  log_info "Attaching registry '${REGISTRY_CONTAINER_HOST}' to network '${network}'"
  docker network connect "$network" "${REGISTRY_CONTAINER_HOST}"
}

# ensure_runner_image_available — make ${IMAGE_NAME}:${IMAGE_TAG} reachable
# from the cluster. Strategy selected by $RUNNER_IMAGE_STRATEGY.
#
# Each strategy writes three outputs read by install_pilot_chart:
#   IMAGE_PULL_POLICY            — runner.image.pullPolicy override
#   HELM_VALUES_FILE             — base values file matching the contract
#   IMAGE_REPOSITORY_FOR_INSTALL — runner.image.repository override
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
      HELM_VALUES_FILE="$(values_file_for_strategy local-k3d-import)"
      IMAGE_REPOSITORY_FOR_INSTALL="$IMAGE_NAME"
      ;;
    registry)
      # Registry strategy owns the registry prefix; reject fully-qualified
      # inputs so users don't accidentally route to an external registry.
      # External registries are out of scope for the in-repo validator.
      if [[ "$IMAGE_NAME" == *"/"* || "$IMAGE_NAME" == *":"* ]]; then
        log_fail "--image-strategy=registry expects a bare --image-repository (got '${IMAGE_NAME}')."
        echo "  The local registry prefix is added automatically." >&2
        echo "  External registries are out of scope here." >&2
        return 1
      fi

      ensure_k3d_registry
      connect_registry_to_cluster

      log_step "Publishing runner image to local registry (${REGISTRY_CONTAINER_HOST})"

      local host_ref="localhost:${REGISTRY_HOST_PORT}/${IMAGE_NAME}:${IMAGE_TAG}"
      local cluster_repo="${REGISTRY_CONTAINER_HOST}:${REGISTRY_CONTAINER_PORT}/${IMAGE_NAME}"

      # Distribution needs a beat after `docker run` on slow machines.
      # Probe /v2/ before pushing so a transient race surfaces as a clear
      # error, not a docker push retry loop.
      local deadline=$((SECONDS + 30))
      local probed=0
      while (( SECONDS < deadline )); do
        if curl -sf -o /dev/null --connect-timeout 2 \
            "http://localhost:${REGISTRY_HOST_PORT}/v2/"; then
          probed=1
          break
        fi
        sleep 0.5
      done
      if (( probed == 0 )); then
        log_fail "Local registry not reachable at localhost:${REGISTRY_HOST_PORT}"
        echo "  Verify container: docker ps --filter name=${REGISTRY_CONTAINER_HOST}" >&2
        echo "  Inspect logs:     docker logs ${REGISTRY_CONTAINER_HOST}" >&2
        return 1
      fi

      docker tag "${IMAGE_NAME}:${IMAGE_TAG}" "$host_ref"

      # Real Docker treats localhost as an insecure registry by default;
      # Podman defaults to HTTPS and requires `--tls-verify=false`. Detect
      # by probing the binary's --help output for the flag itself: this is
      # the direct test of the capability we use, and it correctly
      # identifies the `podman-docker` shim (which self-reports as
      # "docker version" in --version but accepts --tls-verify).
      local -a push_args=()
      if docker push --help 2>&1 | grep -q -- "--tls-verify"; then
        push_args+=("--tls-verify=false")
      fi
      docker push "${push_args[@]}" "$host_ref"

      IMAGE_PULL_POLICY="IfNotPresent"
      HELM_VALUES_FILE="$(values_file_for_strategy registry)"
      IMAGE_REPOSITORY_FOR_INSTALL="$cluster_repo"
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
# `helm upgrade --install` using the strategy-selected values file,
# overriding image fields. Image-strategy choice is resolved beforehand
# by ensure_runner_image_available, which writes HELM_VALUES_FILE,
# IMAGE_REPOSITORY_FOR_INSTALL, and IMAGE_PULL_POLICY.
install_pilot_chart() {
  log_step "Installing Helm chart ${RELEASE_NAME} (namespace ${NAMESPACE})"
  helm upgrade --install "$RELEASE_NAME" \
    "${REPO_ROOT}/deploy/helm/agentium-os/" \
    --namespace "$NAMESPACE" \
    -f "${REPO_ROOT}/${HELM_VALUES_FILE}" \
    --set-string "runner.image.repository=${IMAGE_REPOSITORY_FOR_INSTALL}" \
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
