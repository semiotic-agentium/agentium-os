#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Helm chart rendering check for the runner liveness/readiness probes.
#
# Asserts:
#   1. Default render carries timeoutSeconds=5 and failureThreshold=6 on both
#      probes (the conservative defaults that absorb the runner's slow
#      `/deploy` path under contention).
#   2. A `runner.readinessProbe.timeoutSeconds=10` override is honoured by
#      the template.
#
# Run via:
#   ./scripts/test-helm-runner-probes.sh
#
# This script only needs `helm` (v3 or v4); no cluster access.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHART_DIR="${REPO_ROOT}/deploy/helm/agentium-os"

if ! command -v helm &>/dev/null; then
  echo "FAIL: helm is required" >&2
  exit 1
fi

# Required values that the chart fails closed without. The image repo/tag
# values are placeholders; this script never installs anything.
REQUIRED_VALUES=(
  --set runner.image.repository=agentium-runner
  --set runner.image.tag=test
  --set runner.auth.existingSecret=runner-token
  --set surrealdb.auth.existingSecret=surrealdb-credentials
)

render_runner_statefulset() {
  helm template probe-test "$CHART_DIR" "$@" 2>/dev/null \
    | awk '
        /^kind: StatefulSet$/ { in_ss = 1; buf = $0 ORS; next }
        in_ss && /^---$/ { print buf; in_ss = 0; buf = ""; next }
        in_ss { buf = buf $0 ORS; next }
        END { if (in_ss) print buf }
      ' \
    | awk '
        /name: .*-runner$/ { keep = 1 }
        keep { print }
      '
}

extract_probe_field() {
  # $1: probe key (livenessProbe|readinessProbe)
  # $2: field name (timeoutSeconds|failureThreshold|initialDelaySeconds|periodSeconds)
  local probe="$1"
  local field="$2"
  awk -v probe="$probe" -v field="$field" '
    $0 ~ "^[[:space:]]*" probe ":" { in_probe = 1; next }
    in_probe && /^[[:space:]]*[a-zA-Z]+Probe:/ { in_probe = 0 }
    in_probe && $0 ~ "^[[:space:]]*" field ":" {
      sub(/^[[:space:]]*[a-zA-Z]+:[[:space:]]*/, "")
      print
      exit
    }
  '
}

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$expected" != "$actual" ]]; then
    echo "FAIL: ${label}: expected '${expected}', got '${actual}'" >&2
    return 1
  fi
  echo "PASS: ${label}=${actual}"
}

failures=0

echo "== Default-render probes"
default_render="$(render_runner_statefulset "${REQUIRED_VALUES[@]}")"

assert_eq "livenessProbe.timeoutSeconds (default)" "5" \
  "$(printf '%s\n' "$default_render" | extract_probe_field livenessProbe timeoutSeconds)" \
  || failures=$((failures + 1))
assert_eq "livenessProbe.failureThreshold (default)" "6" \
  "$(printf '%s\n' "$default_render" | extract_probe_field livenessProbe failureThreshold)" \
  || failures=$((failures + 1))
assert_eq "readinessProbe.timeoutSeconds (default)" "5" \
  "$(printf '%s\n' "$default_render" | extract_probe_field readinessProbe timeoutSeconds)" \
  || failures=$((failures + 1))
assert_eq "readinessProbe.failureThreshold (default)" "6" \
  "$(printf '%s\n' "$default_render" | extract_probe_field readinessProbe failureThreshold)" \
  || failures=$((failures + 1))

echo "== Override-render probes (runner.readinessProbe.timeoutSeconds=10)"
override_render="$(render_runner_statefulset \
  "${REQUIRED_VALUES[@]}" \
  --set runner.readinessProbe.timeoutSeconds=10)"

assert_eq "readinessProbe.timeoutSeconds (override)" "10" \
  "$(printf '%s\n' "$override_render" | extract_probe_field readinessProbe timeoutSeconds)" \
  || failures=$((failures + 1))
# Untouched fields should retain their defaults.
assert_eq "readinessProbe.failureThreshold (override leaves untouched)" "6" \
  "$(printf '%s\n' "$override_render" | extract_probe_field readinessProbe failureThreshold)" \
  || failures=$((failures + 1))
assert_eq "livenessProbe.timeoutSeconds (override leaves untouched)" "5" \
  "$(printf '%s\n' "$override_render" | extract_probe_field livenessProbe timeoutSeconds)" \
  || failures=$((failures + 1))

if (( failures > 0 )); then
  echo "FAIL: ${failures} probe assertion(s) failed" >&2
  exit 1
fi

echo "OK: helm runner probe defaults and overrides render as expected"
