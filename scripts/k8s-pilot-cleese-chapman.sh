#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Kubernetes pilot validation for the distributed Cleese/Chapman conversation.
# Assumes the supported Helm chart is already installed.

set -euo pipefail

usage() {
  cat <<'EOF'
Validate the LLM-driven Cleese/Chapman cross-pod conversation on a Helm-installed pilot.

Usage:
  bash scripts/k8s-pilot-cleese-chapman.sh [options]

Options:
  --namespace <ns>       Kubernetes namespace (default: agentium)
  --release <name>       Helm release / instance label (default: agentium)
  --secret <name>        Runner token secret name (default: runner-token)
  --secret-key <key>     Key inside the runner token secret (default: token)
  --fnox-config <name>   ConfigMap containing fnox.toml (default: fnox-config)
  --surreal-secret <name>
                         SurrealDB credential secret (default: surrealdb-credentials)
  --surreal-user-key <key>
                         Username key inside the SurrealDB secret (default: username)
  --surreal-pass-key <key>
                         Password key inside the SurrealDB secret (default: password)
  --runner0-port <port>  Local port for runner-0 port-forward (default: 18081)
  --runner1-port <port>  Local port for runner-1 port-forward (default: 18082)
  --keep-deployed        Leave argument-cleese / argument-chapman deployed
  -h, --help             Show this message and exit

Environment:
  RUNNER_TOKEN           If set, used directly. Otherwise read from the
                         Kubernetes secret named by --secret / --secret-key.

This script enforces the supported credential path:
  - no host .env secrets
  - runners must resolve OPENROUTER_API_KEY from the mounted fnox ConfigMap
EOF
}

NAMESPACE="agentium"
RELEASE_NAME="agentium"
SECRET_NAME="runner-token"
SECRET_KEY="token"
FNOX_CONFIG_NAME="fnox-config"
SURREAL_SECRET_NAME="surrealdb-credentials"
SURREAL_USER_KEY="username"
SURREAL_PASS_KEY="password"
RUNNER0_PORT=18081
RUNNER1_PORT=18082
REMOTE_PORT=18080
KEEP_DEPLOYED=0

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CLI_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/debug/agentium"

RUNNER_STS=""
RUNNER_POD_0=""
RUNNER_POD_1=""
RUNNER_BASE_URL_0=""
RUNNER_BASE_URL_1=""
SURREAL_POD=""
RUNNER_TOKEN="${RUNNER_TOKEN:-}"
SURREAL_USER=""
SURREAL_PASS=""

PF_PID_0=""
PF_PID_1=""
PF_LOG_0=""
PF_LOG_1=""

HASH_CLEESE=""
HASH_CHAPMAN=""
CONTEXT_ID=""
TASK_ID=""
CHAPMAN_REPLY=""

FIXTURE_CLEESE="tests/fixtures/agents/argument-cleese"
FIXTURE_CHAPMAN="tests/fixtures/agents/argument-chapman"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/k8s-pilot-common.sh
source "${SCRIPT_DIR}/lib/k8s-pilot-common.sh"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)      NAMESPACE="$2"; shift 2 ;;
    --release)        RELEASE_NAME="$2"; shift 2 ;;
    --secret)         SECRET_NAME="$2"; shift 2 ;;
    --secret-key)     SECRET_KEY="$2"; shift 2 ;;
    --fnox-config)    FNOX_CONFIG_NAME="$2"; shift 2 ;;
    --surreal-secret) SURREAL_SECRET_NAME="$2"; shift 2 ;;
    --surreal-user-key) SURREAL_USER_KEY="$2"; shift 2 ;;
    --surreal-pass-key) SURREAL_PASS_KEY="$2"; shift 2 ;;
    --runner0-port)   RUNNER0_PORT="$2"; shift 2 ;;
    --runner1-port)   RUNNER1_PORT="$2"; shift 2 ;;
    --keep-deployed)  KEEP_DEPLOYED=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *)                fail "unknown argument: $1" 1 ;;
  esac
done

require_cmd kubectl
require_cmd curl
require_cmd jq
require_cmd cargo
require_cmd awk
require_cmd python3

[[ -d "${REPO_ROOT}/${FIXTURE_CLEESE}" ]] || fail "fixture directory not found: ${FIXTURE_CLEESE}" 1
[[ -d "${REPO_ROOT}/${FIXTURE_CHAPMAN}" ]] || fail "fixture directory not found: ${FIXTURE_CHAPMAN}" 1

cleanup() {
  local code=$?
  if [[ "$KEEP_DEPLOYED" -eq 0 ]]; then
    cleanup_deployments || true
  fi
  cleanup_port_forwards
  exit "$code"
}
trap cleanup EXIT INT TERM

cleanup_port_forwards() {
  if [[ -n "$PF_PID_0" ]] && kill -0 "$PF_PID_0" 2>/dev/null; then
    kill "$PF_PID_0" 2>/dev/null || true
    wait "$PF_PID_0" 2>/dev/null || true
  fi
  if [[ -n "$PF_PID_1" ]] && kill -0 "$PF_PID_1" 2>/dev/null; then
    kill "$PF_PID_1" 2>/dev/null || true
    wait "$PF_PID_1" 2>/dev/null || true
  fi
  [[ -n "$PF_LOG_0" && -f "$PF_LOG_0" ]] && rm -f "$PF_LOG_0"
  [[ -n "$PF_LOG_1" && -f "$PF_LOG_1" ]] && rm -f "$PF_LOG_1"
}

cleanup_deployments() {
  [[ -n "$RUNNER_BASE_URL_0" ]] || return 0
  [[ -n "$RUNNER_BASE_URL_1" ]] || return 0
  undeploy_package "argument-cleese" "$RUNNER_BASE_URL_0" || true
  undeploy_package "argument-cleese" "$RUNNER_BASE_URL_1" || true
  undeploy_package "argument-chapman" "$RUNNER_BASE_URL_0" || true
  undeploy_package "argument-chapman" "$RUNNER_BASE_URL_1" || true
}

wait_for_http() {
  local url="$1" label="$2"
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if curl -sf -o /dev/null --connect-timeout 1 "$url" 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  fail "${label} did not become ready within 30s" 1
}

resolve_single_name() {
  local kind="$1" selector="$2"
  local names
  names="$(
    kubectl -n "$NAMESPACE" get "$kind" -l "$selector" \
      -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null || true
  )"
  names="$(printf '%s\n' "$names" | awk 'NF')"
  local count
  count="$(printf '%s\n' "$names" | awk 'NF {c++} END {print c+0}')"
  if [[ "$count" != "1" ]]; then
    fail "expected exactly one ${kind} for selector '${selector}', found ${count}" 1
  fi
  printf '%s\n' "$names"
}

ensure_fnox_openrouter_default() {
  local fnox_text
  fnox_text="$(
    kubectl -n "$NAMESPACE" get configmap "$FNOX_CONFIG_NAME" \
      -o jsonpath='{.data.fnox\.toml}' 2>/dev/null || true
  )"
  [[ -n "$fnox_text" ]] || fail "ConfigMap ${NAMESPACE}/${FNOX_CONFIG_NAME} is missing fnox.toml" 1
  if ! printf '%s\n' "$fnox_text" | awk '
    /^\[secrets\.OPENROUTER_API_KEY\][[:space:]]*$/ { in_section = 1; next }
    /^\[/                                           { in_section = 0 }
    in_section && /^default[[:space:]]*=/           { found = 1; exit }
    END { exit !found }
  '; then
    fail "fnox-config does not provide [secrets.OPENROUTER_API_KEY] default = ...; update the mounted fnox.toml instead of using host env secrets" 1
  fi
}

build_cli() {
  log "building agentium CLI"
  (cd "$REPO_ROOT" && cargo build -q -p agentium)
}

publish_agent() {
  local agent_dir="$1" repository_url="$2"
  local output hash
  output="$(
    cd "$REPO_ROOT" &&
      "$CLI_BIN" publish \
        --agent-dir "$agent_dir" \
        --repository-url "$repository_url" \
        --runner-token "$RUNNER_TOKEN" 2>&1
  )" || {
    printf '%s\n' "$output" >&2
    return 1
  }
  hash="$(printf '%s\n' "$output" | awk '/^[[:space:]]*hash:/ {print $2}' | tail -n 1)"
  [[ -n "$hash" ]] || {
    printf '%s\n' "$output" >&2
    fail "could not extract publish hash for ${agent_dir}" 2
  }
  printf '%s\n' "$hash"
}

deploy_hash() {
  local hash="$1" base_url="$2"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${RUNNER_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "${base_url}/deploy" >/dev/null
}

undeploy_hash() {
  local hash="$1" base_url="$2"
  curl -sf -X POST \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${RUNNER_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "${base_url}/undeploy" >/dev/null 2>&1 || true
}

undeploy_package() {
  local pkg="$1" base_url="$2"
  local hashes
  hashes="$(
    curl -sf "${base_url}/agents" 2>/dev/null \
      | jq -r --arg pkg "$pkg" '.[] | select(.agent_package == $pkg) | .agent_card.content_hash // empty' 2>/dev/null \
      || true
  )"
  while IFS= read -r hash; do
    [[ -n "$hash" ]] || continue
    undeploy_hash "$hash" "$base_url"
  done <<< "$hashes"
}

jsonrpc_body() {
  local text="$1"
  local millis msg_id corr_id
  millis="$(python3 -c 'import time; print(int(time.time() * 1000))')"
  msg_id="m-${millis}-${RANDOM}"
  corr_id="corr-${millis}-${RANDOM}"
  jq -n --arg text "$text" --arg msg "$msg_id" --arg req "$corr_id" '{
    jsonrpc: "2.0",
    id: $req,
    method: "message.sendStream",
    params: {
      message: {
        messageId: $msg,
        role: "user",
        parts: [{text: $text}]
      }
    }
  }'
}

post_a2a() {
  local base_url="$1" agent_package="$2" text="$3"
  local body
  body="$(jsonrpc_body "$text")"
  # Server returns SSE (text/event-stream); convert each `data: <json>` event into
  # one element of a JSON array so downstream jq filters can treat the response as
  # the historical buffered JSON-RPC frame list.
  curl -sf --max-time 120 \
    -X POST \
    -H "Content-Type: application/json" \
    -d "$body" \
    "${base_url}/agents/${agent_package}/default/a2a" \
    | sed -n 's/^data: //p' \
    | jq -s '.'
}

extract_context_id() {
  jq -r '[
      (.[]? | .result?.chunk?.task?.contextId? // empty),
      (.[]? | .result?.chunk?.contextId? // empty),
      (.[]? | .result?.chunk?.message?.contextId? // empty)
    ] | map(select(type == "string" and length > 0)) | last // empty'
}

extract_task_id() {
  jq -r '[
      (.[]? | .result?.chunk?.task?.id? // empty),
      (.[]? | .result?.chunk?.statusUpdate?.taskId? // empty),
      (.[]? | .result?.chunk?.message?.taskId? // empty)
    ] | map(select(type == "string" and length > 0)) | last // empty'
}

response_text_lines() {
  jq -r '.. | objects | .text? // empty' | awk 'NF'
}

is_argument_line() {
  local text="$1"
  local lower
  lower="$(printf '%s' "$text" | tr '[:upper:]' '[:lower:]')"
  [[ -n "$lower" ]] || return 1
  [[ "${#lower}" -le 96 ]] || return 1
  [[ "$lower" == yes* ]] && return 0
  [[ "$lower" == no* ]] && return 0
  [[ "$lower" == *"n't"* ]] && return 0
  [[ "$lower" == *" not"* ]] && return 0
  [[ "$lower" == *"you did"* ]] && return 0
  [[ "$lower" == *"you do"* ]] && return 0
  [[ "$lower" == *"i didn't"* ]] && return 0
  [[ "$lower" == *"i didnt"* ]] && return 0
  [[ "$lower" == *"certainly"* ]] && return 0
  return 1
}

surreal_query() {
  local sql="$1"
  local raw
  raw="$(
    printf '%s' "$sql" | kubectl exec -n "$NAMESPACE" "$SURREAL_POD" -c surrealdb -i -- \
      /surreal sql \
      --endpoint http://localhost:8000 \
      --username "$SURREAL_USER" \
      --password "$SURREAL_PASS" \
      --namespace cluster \
      --database registry \
      --json 2>/dev/null
  )"
  printf '%s\n' "$raw" | jq '[{result: (.[0] // [])}]'
}

wait_for_history() {
  local url="$1"
  local deadline=$((SECONDS + 20))
  local body=""
  while (( SECONDS < deadline )); do
    body="$(curl -sf "$url" 2>/dev/null || true)"
    if printf '%s' "$body" | jq -e '.items | length > 0' >/dev/null 2>&1; then
      printf '%s\n' "$body"
      return 0
    fi
    sleep 1
  done
  printf '%s\n' "$body"
  return 1
}

wait_for_episode() {
  local url="$1"
  local deadline=$((SECONDS + 20))
  local body=""
  while (( SECONDS < deadline )); do
    body="$(curl -sf "$url" 2>/dev/null || true)"
    if printf '%s' "$body" | jq -e '.task_id != null and (.transcript | length >= 1)' >/dev/null 2>&1; then
      printf '%s\n' "$body"
      return 0
    fi
    sleep 1
  done
  printf '%s\n' "$body"
  return 1
}

log "step 1: resolving cluster objects"
RUNNER_STS="$(resolve_single_name statefulset "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/component=runner")"
RUNNER_POD_0="${RUNNER_STS}-0"
RUNNER_POD_1="${RUNNER_STS}-1"
SURREAL_POD="$(resolve_single_name pod "app.kubernetes.io/instance=${RELEASE_NAME},app.kubernetes.io/component=surrealdb")"

kubectl -n "$NAMESPACE" wait --for=condition=ready "pod/${RUNNER_POD_0}" --timeout=180s >/dev/null
kubectl -n "$NAMESPACE" wait --for=condition=ready "pod/${RUNNER_POD_1}" --timeout=180s >/dev/null

log "step 2: enforcing supported fnox credential path"
ensure_fnox_openrouter_default

log "step 3: resolving runner token"
resolve_runner_token "$NAMESPACE" "$SECRET_NAME" "$SECRET_KEY"

SURREAL_USER="$(
  kubectl -n "$NAMESPACE" get secret "$SURREAL_SECRET_NAME" \
    -o "jsonpath={.data.${SURREAL_USER_KEY}}" 2>/dev/null | base64 -d
)" || fail "could not read secret ${NAMESPACE}/${SURREAL_SECRET_NAME} key=${SURREAL_USER_KEY}" 1
[[ -n "$SURREAL_USER" ]] || fail "SurrealDB username secret is empty" 1

SURREAL_PASS="$(
  kubectl -n "$NAMESPACE" get secret "$SURREAL_SECRET_NAME" \
    -o "jsonpath={.data.${SURREAL_PASS_KEY}}" 2>/dev/null | base64 -d
)" || fail "could not read secret ${NAMESPACE}/${SURREAL_SECRET_NAME} key=${SURREAL_PASS_KEY}" 1
[[ -n "$SURREAL_PASS" ]] || fail "SurrealDB password secret is empty" 1

log "step 4: building CLI and opening pod port-forwards"
build_cli

PF_LOG_0="$(mktemp)"
PF_LOG_1="$(mktemp)"
precheck_local_port_unbound "$RUNNER0_PORT"
precheck_local_port_unbound "$RUNNER1_PORT"
kubectl -n "$NAMESPACE" port-forward "pod/${RUNNER_POD_0}" "${RUNNER0_PORT}:${REMOTE_PORT}" >"$PF_LOG_0" 2>&1 &
PF_PID_0=$!
kubectl -n "$NAMESPACE" port-forward "pod/${RUNNER_POD_1}" "${RUNNER1_PORT}:${REMOTE_PORT}" >"$PF_LOG_1" 2>&1 &
PF_PID_1=$!

RUNNER_BASE_URL_0="http://localhost:${RUNNER0_PORT}"
RUNNER_BASE_URL_1="http://localhost:${RUNNER1_PORT}"

wait_for_http "${RUNNER_BASE_URL_0}/healthz" "runner-0 port-forward"
wait_for_http "${RUNNER_BASE_URL_1}/healthz" "runner-1 port-forward"

log "step 5: cleaning prior deployments"
cleanup_deployments

log "step 6: publishing fixtures to both repositories"
HASH_CLEESE="$(publish_agent "$FIXTURE_CLEESE" "${RUNNER_BASE_URL_0}/repository")"
publish_agent "$FIXTURE_CLEESE" "${RUNNER_BASE_URL_1}/repository" >/dev/null
HASH_CHAPMAN="$(publish_agent "$FIXTURE_CHAPMAN" "${RUNNER_BASE_URL_1}/repository")"
publish_agent "$FIXTURE_CHAPMAN" "${RUNNER_BASE_URL_0}/repository" >/dev/null

log "step 7: deploying Cleese on runner-0 and Chapman on runner-1"
deploy_hash "$HASH_CLEESE" "$RUNNER_BASE_URL_0"
deploy_hash "$HASH_CHAPMAN" "$RUNNER_BASE_URL_1"

log "step 8: verifying placement state in SurrealDB"
placements="$(surreal_query "SELECT agent_package, agent_instance_id, runner_id, runner_endpoint FROM cluster_agent_placements WHERE agent_package IN ['argument-cleese', 'argument-chapman']")"
placement_count="$(printf '%s' "$placements" | jq '[.[] | .result | .[]] | length')"
[[ "$placement_count" == "2" ]] || fail "expected 2 placement rows for argument-cleese / argument-chapman, got ${placement_count}" 1

# Orphan `last_heartbeat_at` column was retired; init_schema must drop it
# from every cluster_runners row so operator queries do not show stale
# `NONE` values that look like broken liveness.
orphan_runners="$(surreal_query "SELECT id FROM cluster_runners WHERE last_heartbeat_at IS NOT NONE")"
orphan_count="$(printf '%s' "$orphan_runners" | jq '[.[] | .result | .[]] | length')"
[[ "$orphan_count" == "0" ]] || fail "expected no cluster_runners rows to expose last_heartbeat_at, got ${orphan_count}" 1

cleese_endpoint="$(printf '%s' "$placements" | jq -r '[.[] | .result | .[] | select(.agent_package == "argument-cleese")] | .[0].runner_endpoint')"
chapman_endpoint="$(printf '%s' "$placements" | jq -r '[.[] | .result | .[] | select(.agent_package == "argument-chapman")] | .[0].runner_endpoint')"
[[ "$cleese_endpoint" == *"runner-0"* ]] || fail "argument-cleese is not placed on runner-0: ${cleese_endpoint}" 1
[[ "$chapman_endpoint" == *"runner-1"* ]] || fail "argument-chapman is not placed on runner-1: ${chapman_endpoint}" 1

# Every placement row must carry runner_id; the UNIQUE index is keyed on it.
missing_runner_id="$(printf '%s' "$placements" | jq -r '[.[] | .result | .[] | select(.runner_id == null)] | length')"
[[ "$missing_runner_id" == "0" ]] || fail "expected every placement row to carry runner_id, got ${missing_runner_id} row(s) missing it" 1

log "step 9: sending A2A request to argument-cleese on runner-0"
response_json="$(post_a2a "$RUNNER_BASE_URL_0" "argument-cleese" "This is a test argument.")"
[[ -n "$response_json" ]] || fail "A2A response was empty" 1

CONTEXT_ID="$(printf '%s' "$response_json" | extract_context_id)"
TASK_ID="$(printf '%s' "$response_json" | extract_task_id)"
if [[ -z "$CONTEXT_ID" ]]; then
  printf '%s\n' "$response_json" >&2
  fail "could not extract contextId from A2A response" 1
fi
if [[ -z "$TASK_ID" ]]; then
  printf '%s\n' "$response_json" >&2
  fail "could not extract taskId from A2A response" 1
fi

response_lines="$(printf '%s' "$response_json" | response_text_lines)"

log "step 10: reading provenance-backed conversation history"
history_url="${RUNNER_BASE_URL_0}/contexts/${CONTEXT_ID}/conversation-history?profile=full"
history_json="$(wait_for_history "$history_url")" || fail "conversation history did not populate for context ${CONTEXT_ID}" 1
history_lines="$(printf '%s' "$history_json" | jq -r '.items[] | select(.content.type == "message") | "\(.role): \(.content.text)"')"
[[ -n "$history_lines" ]] || fail "conversation history for ${CONTEXT_ID} had no message rows" 1

argument_lines=()
while IFS= read -r line; do
  [[ -n "$line" ]] || continue
  text="$line"
  if [[ "$line" == *": "* ]]; then
    text="${line#*: }"
  fi
  if is_argument_line "$text"; then
    argument_lines+=("$text")
  fi
done <<< "$history_lines"

if (( ${#argument_lines[@]} < 2 )); then
  printf '%s\n' "$response_json" >&2
  printf '%s\n' "$history_lines" >&2
  probe_episode="$(curl -sf "${RUNNER_BASE_URL_0}/tasks/${TASK_ID}/episode" 2>/dev/null || true)"
  [[ -n "$probe_episode" ]] && printf '%s\n' "$probe_episode" >&2
  fail "expected at least two argument-style lines in conversation history; got ${#argument_lines[@]}" 1
fi

CHAPMAN_REPLY="${argument_lines[1]}"
printf '%s\n' "$response_lines" | grep -F "$CHAPMAN_REPLY" >/dev/null \
  || fail "returned A2A response did not include Chapman's reply: ${CHAPMAN_REPLY}" 1

log "step 11: reading episode snapshot"
episode_url="${RUNNER_BASE_URL_0}/tasks/${TASK_ID}/episode"
episode_json="$(wait_for_episode "$episode_url")" || fail "episode snapshot did not populate for task ${TASK_ID}" 1
episode_status="$(printf '%s' "$episode_json" | jq -r '.status')"
episode_lines="$(printf '%s' "$episode_json" | jq -r '.transcript[] | select(.content.type == "text") | "\(.role): \(.content.text)"')"

printf '\n'
printf 'Cleese/Chapman validation passed.\n'
printf 'contextId: %s\n' "$CONTEXT_ID"
printf 'taskId: %s\n' "$TASK_ID"
printf 'Chapman reply: %s\n' "$CHAPMAN_REPLY"
printf '\n'
printf 'Placement rows:\n'
printf '  argument-cleese -> %s\n' "$cleese_endpoint"
printf '  argument-chapman -> %s\n' "$chapman_endpoint"
printf '\n'
printf 'Conversation history (%s):\n' "$history_url"
printf '%s\n' "$history_lines"
printf '\n'
printf 'Episode snapshot (%s, status=%s):\n' "$episode_url" "$episode_status"
printf '%s\n' "$episode_lines"
