#!/usr/bin/env bash
# Single entrypoint for the ford-observability demo. Lives inside the demo
# directory so the demo stays self-contained (no repo-level justfile/Make
# pollution).
#
# Usage:
#   ./demo.sh install [helm extra args]
#   ./demo.sh inject
#   ./demo.sh reset
#   ./demo.sh e2e
#
# All commands honor the env knobs documented in each script under scripts/.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS="$SCRIPT_DIR/scripts"

cmd="${1:-}"
shift || true

case "$cmd" in
  install) exec "$SCRIPTS/install.sh" "$@" ;;
  inject)  exec "$SCRIPTS/inject-latency.sh" "$@" ;;
  reset)   exec "$SCRIPTS/reset-demo.sh" "$@" ;;
  e2e)     exec "$SCRIPTS/run-e2e.sh" "$@" ;;
  ""|-h|--help|help)
    cat <<EOF
ford-observability demo

Commands:
  install   helm upgrade --install the chart and wait for rollouts
  inject    POST /admin/failure-mode latency_spike to failure-harness
  reset     stop active failure mode + clear ledger
  e2e       install (unless SKIP_INSTALL=1), inject, wait for coordinator
            context, dump conversation-history/provenance/ledger to OUT_DIR

Env knobs are documented at the top of each scripts/*.sh.
EOF
    [[ -z "$cmd" ]] && exit 1 || exit 0
    ;;
  *)
    echo "unknown command: $cmd" >&2
    exec "$0" --help
    ;;
esac
