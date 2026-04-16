#!/usr/bin/env bash
# Cluster discovery + preflight for the Agentium OS showcase.
# Assumes the k3d cluster from scripts/e2e-k8s/run.sh is already up.
# Sourced by demo.sh; exports constants and helpers the acts depend on.

# ---------------------------------------------------------------------------
# Constants (must match the e2e harness)
# ---------------------------------------------------------------------------
NAMESPACE="agentium"
CLUSTER_NAME="agentium"
RUNNER0_PORT=18081
RUNNER1_PORT=18082
REMOTE_PORT=18080
SURREAL_USER="e2e"
SURREAL_PASS="e2e-test-pass"

# shellcheck disable=SC2034  # used by sourced acts/*.sh
RUNNER0_SVC="http://runner-0.runner.agentium.svc:${REMOTE_PORT}"
# shellcheck disable=SC2034  # used by sourced acts/*.sh
RUNNER1_SVC="http://runner-1.runner.agentium.svc:${REMOTE_PORT}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release/baml-agent-builder"

# Set by discover_cluster_state
RUNNER_TOKEN=""

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

# require_cluster — fail fast if the k3d cluster is not ready for the showcase.
require_cluster() {
  local missing=()
  for cmd in kubectl curl jq k3d; do
    command -v "$cmd" &>/dev/null || missing+=("$cmd")
  done
  if (( ${#missing[@]} > 0 )); then
    die "missing required tools: ${missing[*]}" \
        "install them and re-run"
  fi

  if ! k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"${CLUSTER_NAME}\""; then
    die "k3d cluster '${CLUSTER_NAME}' not found" \
        "first bring it up with: ./scripts/e2e-k8s/run.sh --keep-cluster" \
        "then re-run this showcase"
  fi

  if ! kubectl -n "$NAMESPACE" get pod runner-0 >/dev/null 2>&1; then
    die "runner-0 pod not found in namespace '${NAMESPACE}'" \
        "cluster is up but runners are missing — re-run: ./scripts/e2e-k8s/run.sh --keep-cluster"
  fi

  kubectl -n "$NAMESPACE" wait --for=condition=ready pod/runner-0 --timeout=30s >/dev/null 2>&1 || \
    die "runner-0 is not Ready"
  kubectl -n "$NAMESPACE" wait --for=condition=ready pod/runner-1 --timeout=30s >/dev/null 2>&1 || \
    die "runner-1 is not Ready"

  if [[ ! -x "$BUILDER_BIN" ]]; then
    die "builder binary not found at ${BUILDER_BIN}" \
        "build it with: cargo build --release -p baml-rt-builder --bin baml-agent-builder --all-features"
  fi
}

# discover_cluster_state — read RUNNER_TOKEN from the live StatefulSet.
# The e2e harness randomises the token per run, so we cannot hard-code it.
discover_cluster_state() {
  RUNNER_TOKEN=$(kubectl -n "$NAMESPACE" get statefulset runner \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="RUNNER_TOKEN")].value}' 2>/dev/null)
  if [[ -z "$RUNNER_TOKEN" ]]; then
    die "could not read RUNNER_TOKEN from statefulset/runner" \
        "the cluster may have been created outside the e2e harness"
  fi
}

# ---------------------------------------------------------------------------
# Port-forward lifecycle
# ---------------------------------------------------------------------------

PF_PIDS_SHOWCASE=()

_pf_pidfile() { echo "/tmp/showcase-pf-${1}.pid"; }

# _pf_alive <pod> — is there a live port-forward for this pod?
_pf_alive() {
  local pod="$1" pidfile
  pidfile=$(_pf_pidfile "$pod")
  [[ -f "$pidfile" ]] || return 1
  local pid
  pid=$(cat "$pidfile" 2>/dev/null)
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

# start_pf <pod> <local_port>
start_pf() {
  local pod="$1" port="$2"
  if _pf_alive "$pod"; then
    return 0
  fi
  # Clean up any prior forward, regardless of pidfile state.
  pkill -f "kubectl.*port-forward.*${pod}.*${port}:${REMOTE_PORT}" 2>/dev/null || true
  kubectl -n "$NAMESPACE" port-forward "$pod" "${port}:${REMOTE_PORT}" >/dev/null 2>&1 &
  local pid=$!
  echo "$pid" > "$(_pf_pidfile "$pod")"
  PF_PIDS_SHOWCASE+=("$pid")
  # Wait until the port accepts.
  local deadline=$((SECONDS + 15))
  while (( SECONDS < deadline )); do
    if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:${port}/healthz" 2>/dev/null; then
      return 0
    fi
    sleep 0.2
  done
  die "port-forward for ${pod} did not become ready within 15s"
}

# stop_pf <pod>
stop_pf() {
  local pod="$1" pidfile
  pidfile=$(_pf_pidfile "$pod")
  if [[ -f "$pidfile" ]]; then
    local pid
    pid=$(cat "$pidfile" 2>/dev/null)
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    rm -f "$pidfile"
  fi
  pkill -f "kubectl.*port-forward.*${pod}" 2>/dev/null || true
}

# restart_pf <pod> <local_port> — after a force-kill recreates the pod.
restart_pf() {
  local pod="$1" port="$2"
  stop_pf "$pod"
  kubectl -n "$NAMESPACE" wait --for=condition=ready "pod/${pod}" --timeout=120s >/dev/null 2>&1
  start_pf "$pod" "$port"
}

# kill_all_pf — called from demo.sh trap.
kill_all_pf() {
  for pod in runner-0 runner-1; do
    stop_pf "$pod"
  done
}

# ---------------------------------------------------------------------------
# Platform helpers re-used across acts
# ---------------------------------------------------------------------------

# publish_pkg <fixture> <port> — publish via baml-agent-builder, echo hash.
# Use with care: each call BUMPS the version counter on that runner's
# repository, so two publishes of the same fixture to different runners
# yield DIFFERENT hashes and break migration. Prefer shared_hash below.
publish_pkg() {
  local fixture="$1" port="$2"
  local output
  output=$("$BUILDER_BIN" publish \
    --agent-dir "${REPO_ROOT}/tests/fixtures/agents/${fixture}" \
    --repository-url "http://localhost:${port}/repository" 2>&1) || {
      echo "$output" >&2
      die "publish_pkg ${fixture} to :${port} failed"
    }
  local hash
  hash=$(echo "$output" | grep 'content_hash:' | awk '{print $2}' | tr -d '[:space:]')
  [[ -n "$hash" ]] || die "publish_pkg ${fixture}: no content_hash in builder output"
  echo "$hash"
}

# shared_hash <fixture> — return a content hash that exists on BOTH runners'
# repositories. Content hashes are deterministic given (content, version),
# and version is per-runner-repo, so we find the highest version present on
# both runners and return its (matching) hash. If the fixture is not on
# either runner, publishes to both (runner-0 first, runner-1 second) so the
# counters increment in lockstep and the resulting v1 hash matches.
shared_hash() {
  local fixture="$1"
  local r0_entries r1_entries
  r0_entries=$(curl -sf "http://localhost:${RUNNER0_PORT}/repository/entries" 2>/dev/null || echo '{"entries":[]}')
  r1_entries=$(curl -sf "http://localhost:${RUNNER1_PORT}/repository/entries" 2>/dev/null || echo '{"entries":[]}')

  # Build: highest version on each runner for this fixture.
  local r0_max r1_max
  r0_max=$(echo "$r0_entries" | jq "[.entries[] | select(.version_ref.name==\"${fixture}\") | .version_ref.version] | max // 0")
  r1_max=$(echo "$r1_entries" | jq "[.entries[] | select(.version_ref.name==\"${fixture}\") | .version_ref.version] | max // 0")

  # If either side is empty, we need to publish. Publish in lockstep so counters match.
  if (( r0_max == 0 || r1_max == 0 )); then
    publish_pkg "$fixture" "$RUNNER0_PORT" >/dev/null
    publish_pkg "$fixture" "$RUNNER1_PORT" >/dev/null
    # Re-query.
    r0_entries=$(curl -sf "http://localhost:${RUNNER0_PORT}/repository/entries" 2>/dev/null || echo '{"entries":[]}')
    r1_entries=$(curl -sf "http://localhost:${RUNNER1_PORT}/repository/entries" 2>/dev/null || echo '{"entries":[]}')
    r0_max=$(echo "$r0_entries" | jq "[.entries[] | select(.version_ref.name==\"${fixture}\") | .version_ref.version] | max // 0")
    r1_max=$(echo "$r1_entries" | jq "[.entries[] | select(.version_ref.name==\"${fixture}\") | .version_ref.version] | max // 0")
  fi

  # Pick the highest common version = min(r0_max, r1_max).
  local shared_version=$r0_max
  (( r1_max < shared_version )) && shared_version=$r1_max
  [[ "$shared_version" -ge 1 ]] || die "shared_hash ${fixture}: no common version on both runners"

  # Hash at that version — must match between runners since content+version → hash.
  local r0_hash r1_hash
  r0_hash=$(echo "$r0_entries" | jq -r ".entries[] | select(.version_ref.name==\"${fixture}\" and .version_ref.version==${shared_version}) | .hash")
  r1_hash=$(echo "$r1_entries" | jq -r ".entries[] | select(.version_ref.name==\"${fixture}\" and .version_ref.version==${shared_version}) | .hash")
  [[ "$r0_hash" == "$r1_hash" && -n "$r0_hash" ]] || \
    die "shared_hash ${fixture}: v${shared_version} hashes diverge r0=${r0_hash} r1=${r1_hash}"

  echo "$r0_hash"
}

# deploy_pkg <hash> <port>
deploy_pkg() {
  local hash="$1" port="$2"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${RUNNER_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${port}/deploy" >/dev/null
}

# undeploy_pkg <hash> <port> (best-effort, does not fail the demo on error)
undeploy_pkg() {
  local hash="$1" port="$2"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${RUNNER_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${port}/undeploy" >/dev/null 2>&1 || true
}

# undeploy_by_name <pkg_name> <port> — looks up hash from /agents and undeploys.
undeploy_by_name() {
  local pkg="$1" port="$2"
  local agents hash
  agents=$(curl -sf "http://localhost:${port}/agents" 2>/dev/null || echo "[]")
  hash=$(echo "$agents" | jq -r ".[] | select(.agent_package == \"${pkg}\") | .agent_card.content_hash // empty" 2>/dev/null | head -1)
  if [[ -n "$hash" ]]; then
    undeploy_pkg "$hash" "$port"
  fi
  return 0
}

# clean_agent_state <pkg> — belt-and-suspenders cleanup of all traces of an
# agent across both runners AND the SurrealDB placement table.  Runner-level
# undeploy is best-effort (agent may not be listed after restarts); the DB
# delete ensures no stale placement persists from prior runs.
clean_agent_state() {
  local pkg="$1"
  undeploy_by_name "$pkg" "$RUNNER0_PORT"
  undeploy_by_name "$pkg" "$RUNNER1_PORT"
  surreal_sql "DELETE FROM cluster_agent_placements WHERE agent_package = '${pkg}'" >/dev/null
}

# short_hash <hash> — truncate SHA for readability.
short_hash() {
  echo "${1:0:10}..."
}

# surreal_sql <sql> [namespace] [database] — echo the raw JSON result array.
# Matches the harness's shape: [{result: [...rows...]}]
surreal_sql() {
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
  echo "$raw" | jq '[{result: .[0] // []}]' 2>/dev/null || echo '[{"result":[]}]'
}

# send_a2a <port> <pkg> <text> — send an A2A message, echo SSE body.
send_a2a() {
  local port="$1" pkg="$2" text="$3"
  local millis msg_id corr_id body
  millis=$(python3 -c 'import time; print(int(time.time()*1000))')
  msg_id="demo-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  body=$(cat <<EOF
{"jsonrpc":"2.0","id":"${corr_id}","method":"message.sendStream","params":{"message":{"messageId":"${msg_id}","role":"user","parts":[{"kind":"text","text":"${text}"}]}}}
EOF
)
  curl -sf --max-time 30 -N \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "http://localhost:${port}/agents/${pkg}/default/a2a" 2>/dev/null
}

# send_a2a_ctx <port> <pkg> <text> <context_id> — continue an existing task.
send_a2a_ctx() {
  local port="$1" pkg="$2" text="$3" ctx="$4"
  local millis msg_id corr_id body
  millis=$(python3 -c 'import time; print(int(time.time()*1000))')
  msg_id="demo-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  body=$(cat <<EOF
{"jsonrpc":"2.0","id":"${corr_id}","method":"message.sendStream","params":{"message":{"messageId":"${msg_id}","role":"user","parts":[{"kind":"text","text":"${text}"}],"contextId":"${ctx}"}}}
EOF
)
  curl -sf --max-time 30 -N \
    -X POST \
    -H "Accept: text/event-stream" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "http://localhost:${port}/agents/${pkg}/default/a2a" 2>/dev/null
}

# extract_context_id <sse_body>
extract_context_id() {
  echo "$1" | grep -o '"contextId":"[^"]*"' | head -1 | sed 's/"contextId":"//' | sed 's/"$//'
}
