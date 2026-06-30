#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Adversarial fixture for runner readiness under cgroup CPU throttling.
#
# Constrains the runner pod to runner.resources.limits.cpu=500m and probes
# /readyz + /diagnose at ~100ms cadence during a cpu-peg-agent deploy.
# Asserts four runner-readiness invariants:
#   I1. every /readyz returns an HTTP response (status in {200, 503}) within 1s.
#       Under the runtime-progress-gated contract (#339), 503 during the peg
#       is the correct stall signal; I1 defends transport-level liveness, not
#       the gate's verdict.
#   I2. no probe response is dropped at the TCP level,
#   I3. runtime_progress_lag_ms > 200 for at least one sample,
#   I4. runner-0 has Restart Count == 0 at the point T2 finishes
#       — locks in #369's fix (PR #396): boot-time exit-1 under CPU
#       throttle no longer happens, because the in-process SurrealDB
#       connect retry absorbs the DNS / accept race that CPU throttle
#       used to amplify into a kubelet-visible restart.
# I1/I2/I3 mirror crates/baml-agent-runner/tests/runner_starvation_test.rs
# at the in-process level. I4 has no in-process analogue there — that test
# spawns the runner as a subprocess, with no kubelet to observe restarts.
#
# Usage: bash scripts/e2e-k8s/t2-cgroup-throttle.sh [--no-build] [--keep-cluster]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "${SCRIPT_DIR}/lib.sh"

REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILDER_BIN="${CARGO_TARGET_DIR:-target}/release/agentium""

THROTTLE_OVERLAY="${REPO_ROOT}/deploy/helm/agentium-os/examples/cpu-throttle-test-values.yaml"
FIXTURE_NAME="cpu-peg-agent"
PROBE_DURATION_SECS=15
HEAD_START_SECS=1
BASELINE_DRAIN_SECS=2
SKIP_BUILD=false
KEEP_CLUSTER=false

usage() {
  cat <<'EOF'
Adversarial cgroup-throttled deploy fixture.

Usage:
  bash scripts/e2e-k8s/t2-cgroup-throttle.sh [options]

Options:
  --no-build       Skip Docker image and builder binary builds (reuse cached)
  --keep-cluster   Do not delete the k3d cluster on exit
  -h, --help       Show this message and exit

Exit codes:
  0  all four invariants passed
  1  precondition / transport / bringup failure
  2  invariant assertion failed
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)     SKIP_BUILD=true; shift ;;
    --keep-cluster) KEEP_CLUSTER=true; shift ;;
    -h|--help)      usage; exit 0 ;;
    *)              log_fail "unknown argument: $1"; exit 1 ;;
  esac
done

LOG_DIR="${REPO_ROOT}/e2e-k8s-logs/t2-$(date +%Y%m%d-%H%M%S)"
SAMPLES_DIR="$LOG_DIR/samples"
STOP_FILE="$LOG_DIR/probers.stop"
mkdir -p "$SAMPLES_DIR"

PROBER_PIDS=()
# Set to 1 by main() if POST /deploy returned 2xx. assert_invariants reads
# this to decide whether I3 (max lag > 200ms) is assertable: without a
# successful /deploy, no cpu-peg ran, so a low max_lag isn't evidence the
# meter is blind.
DEPLOY_OK=0

# Capture cgroup/memory/pod state from the runner pod before cluster teardown,
# so the decision matrix in issue #350 can be populated on every run regardless
# of pass/fail. Runs unconditionally; failures swallowed since the cluster may
# be in any state by the time we land here.
capture_diagnostics() {
  if [[ -z "${RUNNER_POD_0:-}" ]]; then
    return 0
  fi
  local diag_dir="$LOG_DIR/diagnostics"
  mkdir -p "$diag_dir"
  log_step "Capturing cgroup diagnostics from ${RUNNER_POD_0}"
  # --request-timeout caps each kubectl call so a transitional pod state
  # (e.g. just SIGKILLed but not yet restarted) can't stretch cleanup into
  # several wasted minutes of dead-wait on a CI run.
  local k_timeout="--request-timeout=10s"
  kubectl exec "$k_timeout" -n "$NAMESPACE" "$RUNNER_POD_0" -- cat /sys/fs/cgroup/cpu.stat \
    > "$diag_dir/cpu.stat" 2>"$diag_dir/cpu.stat.err" || true
  kubectl exec "$k_timeout" -n "$NAMESPACE" "$RUNNER_POD_0" -- cat /sys/fs/cgroup/memory.events \
    > "$diag_dir/memory.events" 2>"$diag_dir/memory.events.err" || true
  kubectl describe pod "$k_timeout" -n "$NAMESPACE" "$RUNNER_POD_0" \
    > "$diag_dir/pod-describe.txt" 2>"$diag_dir/pod-describe.err" || true
  log_info "Diagnostics written to $diag_dir/"
}

cleanup() {
  local code=$?
  for pid in "${PROBER_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  kill_all_port_forwards
  capture_diagnostics
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
  # The probe loop's now_ms() relies on EPOCHREALTIME (bash 5.0+, 2018).
  # Catch ancient bash here rather than after a 5+ minute build pipeline.
  if (( BASH_VERSINFO[0] < 5 )); then
    log_fail "bash 5.0+ required (have ${BASH_VERSION}); macOS /bin/bash is 3.2 — install via homebrew and re-run."
    exit 1
  fi
  local missing=()
  for cmd in docker k3d kubectl helm jq curl cargo awk; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    log_fail "Missing required tools: ${missing[*]}"
    exit 1
  fi
  if [[ ! -f "$THROTTLE_OVERLAY" ]]; then
    log_fail "Throttle overlay not found: $THROTTLE_OVERLAY"
    exit 1
  fi
  log_info "All preflight checks passed."
}

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
  log_step "Building runner Docker image (${IMAGE_NAME}:${IMAGE_TAG})"
  ensure_image_tag_or_nonce
  docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" "$REPO_ROOT"
}

setup_cluster() {
  log_step "Creating k3d cluster '${CLUSTER_NAME}'"
  if k3d cluster list -o json 2>/dev/null | grep -q "\"name\":\"${CLUSTER_NAME}\""; then
    log_info "Cluster '${CLUSTER_NAME}' already exists — deleting for clean run."
    k3d cluster delete "$CLUSTER_NAME"
  fi
  k3d cluster create --config "${REPO_ROOT}/deploy/k3d/cluster.yaml"

  ensure_runner_image_available
  create_pilot_objects
  write_scenario_values "$THROTTLE_OVERLAY"
  install_pilot_release

  log_step "Starting port-forward to ${RUNNER_POD_0} on localhost:${RUNNER0_PORT}"
  start_port_forward "$RUNNER_POD_0" "$RUNNER0_PORT" "$REMOTE_PORT"
}

# Millisecond timestamp from bash 5's EPOCHREALTIME — no fork per call.
now_ms() {
  local r=${EPOCHREALTIME/./}
  echo $(( r / 1000 ))
}

# probe_endpoint <url> <out_file> <duration_ms>
#
# Background probe loop. Hits <url> at ~100ms cadence, capturing one line
# per probe in <out_file>:
#   <poll_offset_ms> <status_or_ERR> <elapsed_ms> <lag_ms_or_NA>
#
# `<status>` is the HTTP status code on a successful exchange, or `ERR<code>`
# where <code> is curl's exit code on a transport-level failure. `<lag_ms>`
# is `runtime_progress_lag_ms` parsed from the JSON body when the URL is
# /diagnose; otherwise `NA`.
probe_endpoint() {
  local url="$1" out="$2" duration_ms="$3"
  local body_file
  body_file=$(mktemp)
  local start_ms
  start_ms="$(now_ms)"
  : > "$out"
  while true; do
    local poll_offset
    poll_offset=$(( $(now_ms) - start_ms ))
    if (( poll_offset >= duration_ms )); then break; fi
    if [[ -f "$STOP_FILE" ]]; then break; fi

    local probe_start probe_meta rc status time_total elapsed_ms lag
    probe_start="$(now_ms)"
    probe_meta=$(curl -s -o "$body_file" --max-time 1 \
      -w '%{http_code} %{time_total}' "$url" 2>/dev/null) && rc=0 || rc=$?
    if (( rc == 0 )); then
      read -r status time_total <<< "$probe_meta"
      elapsed_ms=$(awk -v s="$time_total" 'BEGIN{ printf "%.0f", s*1000 }')
      lag="NA"
      if [[ "$url" == *"/diagnose" ]]; then
        local parsed
        parsed=$(jq -r '.runtime_progress_lag_ms // empty' "$body_file" 2>/dev/null || true)
        [[ -n "$parsed" ]] && lag="$parsed"
      fi
      printf '%s %s %s %s\n' "$poll_offset" "$status" "$elapsed_ms" "$lag" >> "$out"
    else
      printf '%s ERR%s 0 NA\n' "$poll_offset" "$rc" >> "$out"
    fi

    local probe_elapsed sleep_ms
    probe_elapsed=$(( $(now_ms) - probe_start ))
    sleep_ms=$(( 100 - probe_elapsed ))
    if (( sleep_ms > 0 )); then
      # Bash printf is fork-free; sleep_ms < 1000 by construction here.
      sleep "$(printf '0.%03d' "$sleep_ms")"
    fi
  done
  rm -f "$body_file"
}

start_probers() {
  rm -f "$STOP_FILE"
  probe_endpoint "http://localhost:${RUNNER0_PORT}/readyz"   "$SAMPLES_DIR/readyz.samples"   $((PROBE_DURATION_SECS * 1000)) &
  PROBER_PIDS+=($!)
  probe_endpoint "http://localhost:${RUNNER0_PORT}/diagnose" "$SAMPLES_DIR/diagnose.samples" $((PROBE_DURATION_SECS * 1000)) &
  PROBER_PIDS+=($!)
}

stop_probers() {
  touch "$STOP_FILE"
  for pid in "${PROBER_PIDS[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
  PROBER_PIDS=()
}

assert_invariants() {
  local readyz="$SAMPLES_DIR/readyz.samples"
  local diagnose="$SAMPLES_DIR/diagnose.samples"

  if [[ ! -s "$readyz" ]]; then
    log_fail "no /readyz samples collected"
    return 1
  fi
  if [[ ! -s "$diagnose" ]]; then
    log_fail "no /diagnose samples collected"
    return 1
  fi

  local readyz_count diagnose_count
  readyz_count=$(wc -l < "$readyz")
  diagnose_count=$(wc -l < "$diagnose")
  log_info "Collected ${readyz_count} /readyz samples, ${diagnose_count} /diagnose samples"

  # ── Invariant 1: every /readyz probe returns an HTTP response within 1s ─
  local i1_failed=0
  while read -r offset status elapsed _lag; do
    if [[ "$status" == ERR* ]]; then
      log_fail "I1: /readyz probe at offset ${offset}ms hit transport error: ${status}"
      i1_failed=1
      continue
    fi
    if [[ "$status" != "200" && "$status" != "503" ]]; then
      log_fail "I1: /readyz probe at offset ${offset}ms returned status ${status}; expected 200 or 503 (gate verdict)"
      i1_failed=1
      continue
    fi
    if (( elapsed >= 1000 )); then
      log_fail "I1: /readyz probe at offset ${offset}ms took ${elapsed}ms; must respond within 1000ms"
      i1_failed=1
    fi
  done < "$readyz"

  # ── Invariant 2: no transport-level drops on /diagnose ─────────────────
  local i2_failed=0
  while read -r offset status _elapsed _lag; do
    if [[ "$status" == ERR* ]]; then
      log_fail "I2: /diagnose probe at offset ${offset}ms hit transport error: ${status}"
      i2_failed=1
    fi
  done < "$diagnose"

  # ── Invariant 3: at least one /diagnose sample shows lag > 200ms ───────
  local max_lag=0
  local lag_count=0
  while read -r _offset status _elapsed lag; do
    [[ "$status" == ERR* ]] && continue
    [[ "$lag" == "NA" || -z "$lag" ]] && continue
    lag_count=$(( lag_count + 1 ))
    if (( lag > max_lag )); then
      max_lag="$lag"
    fi
  done < "$diagnose"

  if (( DEPLOY_OK == 0 )); then
    log_info "I3: /deploy did not accept; runtime_progress_lag_ms not asserted (max observed ${max_lag}ms across ${lag_count} samples)"
  elif (( lag_count == 0 )); then
    log_fail "I3: no runtime_progress_lag_ms values parsed from /diagnose; body shape may have drifted"
  elif (( max_lag <= 200 )); then
    log_fail "I3: expected runtime_progress_lag_ms > 200 in at least one /diagnose sample, but max observed was ${max_lag}ms across ${lag_count} samples"
  else
    log_pass "I3: max runtime_progress_lag_ms = ${max_lag}ms across ${lag_count} samples (> 200ms threshold)"
  fi

  if (( i1_failed == 0 )); then
    log_pass "I1: every /readyz probe returned 200 or 503 within 1s (${readyz_count} samples)"
  fi
  if (( i2_failed == 0 )); then
    log_pass "I2: no /diagnose transport-level drops (${diagnose_count} samples)"
  fi

  # ── Invariant 4: runner-0 has not restarted before T2 finishes ─────────
  local restart_count
  if ! restart_count=$(kubectl --request-timeout=10s -n "$NAMESPACE" \
      get pod "$RUNNER_POD_0" \
      -o jsonpath='{.status.containerStatuses[0].restartCount}' 2>&1); then
    log_fail "I4: kubectl get pod failed; cannot evaluate restartCount (test-infrastructure issue, not a runner regression): $restart_count"
  elif [[ "$restart_count" == "0" ]]; then
    log_pass "I4: $RUNNER_POD_0 restartCount == 0 (no boot-time restart)"
  else
    log_fail "I4: $RUNNER_POD_0 restartCount == $restart_count; expected 0. Capture 'kubectl -n $NAMESPACE logs $RUNNER_POD_0 --previous' for the failing-boot log."
  fi
}

main() {
  echo "=== Agentium OS cgroup-throttled deploy harness ==="
  echo ""

  preflight
  build_phase
  setup_cluster
  local hash
  hash=$(publish_fixture "$FIXTURE_NAME" "$RUNNER0_PORT")
  log_info "Published ${FIXTURE_NAME}: ${hash}"

  # Drain any baseline lag from boot so probe samples reflect deploy load.
  log_step "Draining baseline lag (${BASELINE_DRAIN_SECS}s)"
  sleep "$BASELINE_DRAIN_SECS"

  log_step "Starting probers (~100ms cadence, ${PROBE_DURATION_SECS}s window)"
  start_probers
  sleep "$HEAD_START_SECS"

  log_step "Deploying ${FIXTURE_NAME}"
  # Roll a local POST instead of lib.sh:deploy_hash because the latter uses
  # `curl -sf` which silently drops the 5xx body — under cgroup throttle
  # the failure mode is exactly an in-flight 5xx, and the body / HTTP code
  # are the only signals that distinguish a starved runner from a probe
  # mismatch. On a non-2xx response: log the failure but keep probing for
  # the full window so I1/I2 still report on what they captured. I3 is
  # downgraded to informational by assert_invariants (no cpu-peg = no lag
  # signal to assert against).
  local deploy_body deploy_code
  deploy_body=$(mktemp)
  deploy_code=$(curl -s --max-time 30 -X POST \
    -o "$deploy_body" -w '%{http_code}' \
    -H "Content-Type: application/json" \
    -H "X-Runner-Token: ${E2E_TOKEN}" \
    -d "{\"hash\":\"${hash}\"}" \
    "http://localhost:${RUNNER0_PORT}/deploy" 2>/dev/null) || deploy_code="000"
  if [[ "$deploy_code" == 2* ]]; then
    DEPLOY_OK=1
    log_info "/deploy accepted (HTTP ${deploy_code})"
  else
    log_fail "/deploy of ${FIXTURE_NAME} failed (HTTP ${deploy_code}): $(cat "$deploy_body")"
  fi
  rm -f "$deploy_body"

  # cpu-peg-agent holds the JS thread for ~5s; let probers ride out the
  # remaining window so /diagnose can integrate post-deploy lag.
  log_step "Holding while probers complete"
  sleep $((PROBE_DURATION_SECS - HEAD_START_SECS - BASELINE_DRAIN_SECS))

  stop_probers

  log_step "Asserting invariants"
  assert_invariants

  if (( HAS_FAILURE )); then
    log_fail "Invariant assertions failed — see ${LOG_DIR}/ for samples and pod logs"
    exit 2
  fi
  log_pass "All four invariants passed"
}

main "$@"
