# Shared helpers for Kubernetes pilot operator scripts that use the
# `==>` / `fail-with-exit` convention.
#
# Source from the caller's entry section:
#   SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
#   # shellcheck source=lib/k8s-pilot-common.sh
#   source "${SCRIPT_DIR}/lib/k8s-pilot-common.sh"
#
# Does not set -e/-u/pipefail or install traps; callers manage those.
# A separate library at scripts/e2e-k8s/lib.sh serves the e2e-harness
# scripts with a different convention (`[INFO]` / `[FAIL]` prefixes,
# `log_fail` without exit, HAS_FAILURE accumulator).

log()  { printf '==> %s\n' "$*"; }
warn() { printf '  ! %s\n' "$*" >&2; }
fail() { printf '  x %s\n' "$1" >&2; exit "${2:-1}"; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 \
    || fail "required command not found: $1 (install it and re-run)" 1
}

# Enumerate every pod from a Helm release into a bash array variable. Uses
# the standard `app.kubernetes.io/instance` label set by the chart.
#
# Args: namespace, release_name, array_var_name
# Example: discover_release_pods agentium agentium pods
#          for pod in "${pods[@]}"; do ... done
discover_release_pods() {
  local namespace="$1" release_name="$2" array_var="$3"
  local raw
  raw="$(kubectl -n "$namespace" get pods \
    -l "app.kubernetes.io/instance=${release_name}" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)"
  if [[ -z "$raw" ]]; then
    fail "no pods found for release '${release_name}' in namespace '${namespace}'" 1
  fi
  # shellcheck disable=SC2206  # jsonpath emits space-separated names; word-split is intentional
  eval "$array_var=( \$raw )"
}

# Resolve RUNNER_TOKEN: honour the env var if set, otherwise read from the
# named Kubernetes secret. Exports RUNNER_TOKEN; fails on missing/empty.
#
# Args: namespace, secret_name, secret_key
resolve_runner_token() {
  local namespace="$1" secret_name="$2" secret_key="$3"
  if [[ -z "${RUNNER_TOKEN:-}" ]]; then
    if ! RUNNER_TOKEN="$(kubectl -n "$namespace" get secret "$secret_name" \
          -o "jsonpath={.data.$secret_key}" 2>/dev/null | base64 -d)"; then
      fail "could not read secret ${namespace}/${secret_name} key=${secret_key}. Set RUNNER_TOKEN or pass --secret." 1
    fi
    if [[ -z "$RUNNER_TOKEN" ]]; then
      fail "secret ${namespace}/${secret_name} key=${secret_key} is empty" 1
    fi
  fi
  export RUNNER_TOKEN
}

# Fail if `localhost:$local_port` already serves `/healthz` — a stale dev
# runner or concurrent port-forward would otherwise silently capture the
# operator script's traffic.
precheck_local_port_unbound() {
  local local_port="$1"
  if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:$local_port/healthz" 2>/dev/null; then
    fail "localhost:$local_port already responds to /healthz — another process is bound here. Stop it or re-run with a different port." 1
  fi
}

# Watch `pf_pid` and poll `localhost:$local_port/healthz` for up to 30s.
# Fail on kubectl crash or timeout, dumping `pf_log` in the message.
#
# Args: pf_pid, local_port, pf_log
wait_pilot_port_forward_ready() {
  local pf_pid="$1" local_port="$2" pf_log="$3"
  for _ in $(seq 1 60); do
    if ! kill -0 "$pf_pid" 2>/dev/null; then
      fail "kubectl port-forward exited before becoming ready: $(cat "$pf_log")" 1
    fi
    if curl -sf -o /dev/null --connect-timeout 1 "http://localhost:$local_port/healthz" 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  fail "port-forward did not become ready within 30s: $(cat "$pf_log")" 1
}

# Fetch /cluster/agents into `response_file`; echo the HTTP status code.
# `--retry 2 --retry-connrefused --retry-delay 1` absorbs transient blips
# during the per-peer fan-out inside the handler (5s per-runner timeout).
#
# Args: runner_url, runner_token, response_file
fetch_cluster_agents() {
  local runner_url="$1" runner_token="$2" response_file="$3"
  curl -sS -o "$response_file" -w '%{http_code}' \
    --retry 2 --retry-connrefused --retry-delay 1 \
    -H "X-Runner-Token: $runner_token" \
    "$runner_url/cluster/agents" || true
}

# Default threshold for `assert_heartbeat_freshness` — six heartbeat
# intervals (the runner heartbeats every 5s). The directory query already
# filters out placements whose runner heartbeat is past `placement_ttl_ms`
# (default 90s, so dead runners surface as orphans via I2). I4 catches a
# narrower regression: a runner that is still heartbeating but slow.
DEFAULT_HEARTBEAT_FRESHNESS_THRESHOLD_MS=30000

# Assert heartbeat freshness from a fetched /cluster/agents response.
# Fails with `[FAIL I4]` and exit 2 on:
#   - any runner with `last_heartbeat_ms` older than $threshold_ms vs `now`
#   - any non-orphan runner where `last_heartbeat_ms` is absent (the
#     heartbeat task never wrote, which would be a regression)
#
# Args: response_file, threshold_ms (optional; defaults to 30000)
assert_heartbeat_freshness() {
  local response_file="$1"
  local threshold_ms="${2:-$DEFAULT_HEARTBEAT_FRESHNESS_THRESHOLD_MS}"
  local stale_detail never_detail messages=()
  stale_detail="$(jq -r --argjson t "$threshold_ms" \
    '[.runners[] | select(.last_heartbeat_ms != null) | select((now * 1000 - .last_heartbeat_ms) > $t) | "\(.runner_id) (lag=\(((now * 1000 - .last_heartbeat_ms) | floor) | tostring)ms)"] | join(", ")' \
    "$response_file")"
  if [[ -n "$stale_detail" ]]; then
    messages+=("stale heartbeats (>${threshold_ms}ms): $stale_detail")
  fi
  # A live runner row (not an orphan placement) with no last_heartbeat_ms
  # would mean the directory returned a row without ever writing a heartbeat.
  never_detail="$(jq -r \
    '[.runners[] | select(.last_heartbeat_ms == null) | select((.error // "") | contains("orphan placement") | not) | .runner_id] | join(", ")' \
    "$response_file")"
  if [[ -n "$never_detail" ]]; then
    messages+=("live runners with no last_heartbeat_ms (heartbeat task never wrote): $never_detail")
  fi
  if [[ "${#messages[@]}" -gt 0 ]]; then
    # `${arr[*]}` with multi-char IFS collapses to the first char only — join
    # explicitly with printf so multi-error operator output stays readable.
    local joined
    printf -v joined ' | %s' "${messages[@]}"
    fail "[FAIL I4] ${joined# | }" 2
  fi
}

# Assert placement consistency from a fetched /cluster/agents response.
# Fails with `[FAIL I{1,2,3}]` and exit 2 on:
#   I1 — any runner with `reachable: false`
#   I2 — any runner whose `error` contains "orphan placement"
#   I3 — any agent row with `version_skew: true`
#
# Args: response_file
assert_placement_consistency() {
  local response_file="$1"
  local unreachable_count detail orphan_count skew_count
  unreachable_count="$(jq '[.runners[] | select(.reachable == false)] | length' "$response_file")"
  if [[ "$unreachable_count" -gt 0 ]]; then
    detail="$(jq -r '[.runners[] | select(.reachable == false) | "\(.runner_id) (\(.error))"] | join(", ")' "$response_file")"
    fail "[FAIL I1] $unreachable_count runner(s) unreachable from /cluster/agents fan-out: $detail" 2
  fi
  orphan_count="$(jq '[.runners[] | select(.error and (.error | contains("orphan placement")))] | length' "$response_file")"
  if [[ "$orphan_count" -gt 0 ]]; then
    detail="$(jq -r '[.runners[] | select(.error and (.error | contains("orphan placement"))) | .runner_id] | join(", ")' "$response_file")"
    fail "[FAIL I2] $orphan_count orphan placement(s) found (placement-table runner_id not in cluster_runners): $detail" 2
  fi
  skew_count="$(jq '[.agents[] | select(.version_skew == true)] | length' "$response_file")"
  if [[ "$skew_count" -gt 0 ]]; then
    detail="$(jq -r '[.agents[] | select(.version_skew == true) | "\(.agent_package)/\(.agent_instance_id)"] | join(", ")' "$response_file")"
    fail "[FAIL I3] $skew_count agent(s) with version skew across placements: $detail" 2
  fi
}
