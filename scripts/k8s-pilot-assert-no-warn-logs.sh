#!/usr/bin/env bash
# No-unexpected-WARN log assertion for the Kubernetes pilot rehearsal. See `--help`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/k8s-pilot-common.sh
source "${SCRIPT_DIR}/lib/k8s-pilot-common.sh"

usage() {
  cat <<'EOF'
No-unexpected-WARN log assertion for the Kubernetes pilot rehearsal.

Scans `kubectl logs` of every pod from the Helm release for WARN-level
lines and exits non-zero if any line is found that doesn't match an
explicit allowlist. Catches the regression class where the cluster runs
fine end-to-end but logs reveal a subtle issue (e.g. a degraded subsystem
that hasn't yet broken a request path).

The allowlist is intentionally narrow. Add an entry only when the WARN is
both known-harmless and recurrent enough to be operationally tolerable.

Usage:
  bash scripts/k8s-pilot-assert-no-warn-logs.sh [options]

Options:
  --namespace <ns>     Kubernetes namespace (default: agentium)
  --release <name>     Helm release / instance label (default: agentium)
  --extra-allow <re>   Append an extra regex to the allowlist (repeatable).
                       Useful for one-off operator runs where a known
                       transient WARN should not fail the assertion.
  -h, --help           Show this message and exit

Exit codes:
  0  no unexpected WARN log lines
  1  precondition or transport failure
  2  unexpected WARN log lines detected
EOF
}

NAMESPACE="agentium"
RELEASE_NAME="agentium"
EXTRA_ALLOW=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace)   NAMESPACE="$2"; shift 2 ;;
    --release)     RELEASE_NAME="$2"; shift 2 ;;
    --extra-allow) EXTRA_ALLOW+=("$2"); shift 2 ;;
    -h|--help)     usage; exit 0 ;;
    *)             fail "unknown argument: $1" 1 ;;
  esac
done

require_cmd kubectl

# Allowlist: extended-regex patterns matched against each WARN-line. Keep
# narrow — every entry here is an operator promise that the WARN is known,
# bounded, and harmless.
WARN_ALLOWLIST=(
  # Boot-time SurrealDB connect retry: the runner starts before the
  # SurrealDB service is fully accepting connections, retries with
  # exponential backoff (max 6 attempts), and succeeds — emitted from
  # baml_agent_runner's startup config-store wiring.
  'remote config store connect failed; retrying after delay'
)
WARN_ALLOWLIST+=("${EXTRA_ALLOW[@]}")

# Empty allowlist would make `grep -Ev ""` match every line as expected — but
# we still want a clear failure mode if a future maintainer deletes the
# hardcoded entry. Refuse to run with no allowlist.
if [[ "${#WARN_ALLOWLIST[@]}" -eq 0 ]]; then
  fail "WARN_ALLOWLIST is empty — refusing to run (grep -Ev \"\" would silently pass). Add an entry or pass --extra-allow." 1
fi

# Build the grep-extended-regex alternation once; reused across every pod.
ALLOWED_PATTERN="$(IFS='|'; printf '%s' "${WARN_ALLOWLIST[*]}")"

# Print any WARN lines from $pod that do not match the allowlist. The
# `grep WARN` filter runs before the ANSI-strip sed so the sed only sees
# matching lines (negligible for 3 pods, MB-saving for long-lived clusters).
scan_pod() {
  local pod="$1"
  kubectl -n "$NAMESPACE" logs "$pod" 2>&1 \
    | grep -E 'WARN' \
    | sed -E 's/\x1b\[[0-9;]*m//g' \
    | grep -Ev "$ALLOWED_PATTERN" \
    || true
}

log "discovering pods in release '$RELEASE_NAME' (namespace '$NAMESPACE')"
discover_release_pods "$NAMESPACE" "$RELEASE_NAME" pods

log "scanning ${#pods[@]} pod(s) for unexpected WARN lines"
# Build per-pod report; collect into a single fail message at the end so
# operators see every offending pod on one CI run, not just the first.
unexpected_report=""
unexpected_count=0
for pod in "${pods[@]}"; do
  out="$(scan_pod "$pod")"
  if [[ -n "$out" ]]; then
    pod_count="$(printf '%s\n' "$out" | wc -l | tr -d ' ')"
    unexpected_count=$((unexpected_count + pod_count))
    unexpected_report+="--- $pod ($pod_count line(s)) ---"$'\n'"$out"$'\n'
  fi
done

if [[ -n "$unexpected_report" ]]; then
  printf '%s\n' "$unexpected_report" >&2
  fail "[FAIL I5] $unexpected_count unexpected WARN log line(s) across the cluster (see above)" 2
fi

log "OK — no unexpected WARN lines (allowlist size: ${#WARN_ALLOWLIST[@]})"
