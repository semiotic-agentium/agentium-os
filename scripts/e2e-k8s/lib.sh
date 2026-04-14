#!/usr/bin/env bash
# Shared helpers for the E2E k8s test harness.
# Sourced by run.sh — not executed directly.

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
NAMESPACE="agentium"
CLUSTER_NAME="agentium"
IMAGE_NAME="agentium-runner"
IMAGE_TAG="e2e"
SURREAL_USER="e2e"
SURREAL_PASS="e2e-test-pass"
E2E_TOKEN="e2e-token-${RANDOM}"
RUNNER0_PORT=18081
RUNNER1_PORT=18082
REMOTE_PORT=18080

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
  output=$("$BUILDER_BIN" publish \
    --agent-dir "${REPO_ROOT}/tests/fixtures/agents/${fixture}" \
    --repository-url "http://localhost:${port}/repository" 2>&1)
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
  hash=$(echo "$agents" | jq -r ".[] | select(.agent_package == \"${pkg}\") | .content_hash // empty" 2>/dev/null | head -1)
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
# Executes a SurrealQL statement via kubectl exec on surrealdb-0.
# Defaults: namespace=cluster, database=registry.
surreal_query() {
  local sql="$1"
  local ns="${2:-cluster}"
  local db="${3:-registry}"
  local raw
  raw=$(echo "$sql" | kubectl exec -n "$NAMESPACE" surrealdb-0 -c surrealdb -i -- \
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
  local msg_id
  msg_id="e2e-$(date +%s)-${RANDOM}"
  cat <<JSONEOF
{"jsonrpc":"2.0","id":1,"method":"message.sendStream","params":{"message":{"messageId":"${msg_id}","role":"user","parts":[{"kind":"text","text":"${text}"}]}}}
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
    "http://localhost:${port}/agents/${pkg}/default/a2a/sse" 2>/dev/null
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
  for pod in runner-0 runner-1 surrealdb-0; do
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

# has_llm_keys — returns 0 if the local .env has a non-empty OPENROUTER_API_KEY.
has_llm_keys() {
  local repo_root
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  [[ -f "${repo_root}/.env" ]] \
    && grep -qE '^OPENROUTER_API_KEY="?[^"]+' "${repo_root}/.env"
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
    if [[ "$st" == "PASS" ]]; then ((passed++)); else ((failed++)); fi
  done
  echo ""
  echo "${passed}/$((passed + failed)) passed, ${failed}/$((passed + failed)) failed"
  if (( failed > 0 )) && [[ -n "$LOG_DIR" ]]; then
    echo "Logs: ${LOG_DIR}/"
  fi
}
