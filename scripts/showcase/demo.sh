#!/usr/bin/env bash
# Agentium OS — live terminal showcase.
#
# Five acts, each ~90 seconds, each demonstrating one differentiator:
#
#   1. The cluster placement table IS the service mesh
#   2. Host-governed safety — the router refuses unsafe requests
#   3. The audit trail outlives the pod
#   4. Conversations outlive their pods
#   5. Dead runners exit routing in seconds, not kubelet cycles
#
# The same five properties are asserted by scripts/e2e-k8s/run.sh
# scenarios 3, 5-6, 12, 15, and 13 respectively. This showcase is
# the narrated, presenter-paced version of the same evidence.
#
# Usage:
#   ./scripts/showcase/demo.sh                 # paced — press Enter between acts
#   ./scripts/showcase/demo.sh --auto          # continuous (for recording)
#   ./scripts/showcase/demo.sh --dry-run       # narration + commands only
#   ./scripts/showcase/demo.sh --act 3         # run a single act (1..5)
#   ./scripts/showcase/demo.sh --help
#
# Prerequisite: the e2e k3d cluster must be up.
#   ./scripts/e2e-k8s/run.sh --keep-cluster    # (one-time, ~6 minutes)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=narrative.sh
source "${SCRIPT_DIR}/narrative.sh"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"
# shellcheck source=acts/01_mesh.sh
source "${SCRIPT_DIR}/acts/01_mesh.sh"
# shellcheck source=acts/02_safety.sh
source "${SCRIPT_DIR}/acts/02_safety.sh"
# shellcheck source=acts/03_provenance.sh
source "${SCRIPT_DIR}/acts/03_provenance.sh"
# shellcheck source=acts/04_continuity.sh
source "${SCRIPT_DIR}/acts/04_continuity.sh"
# shellcheck source=acts/05_selfheal.sh
source "${SCRIPT_DIR}/acts/05_selfheal.sh"

# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------
SINGLE_ACT=""
SHOWCASE_AUTO=false
SHOWCASE_DRY_RUN=false

print_help() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

while (( $# > 0 )); do
  case "$1" in
    --auto)     SHOWCASE_AUTO=true ;;
    --dry-run)  SHOWCASE_DRY_RUN=true ;;
    --act)      SINGLE_ACT="${2:-}"; shift ;;
    --help|-h)  print_help; exit 0 ;;
    *)          echo "unknown flag: $1" >&2; echo "try --help" >&2; exit 2 ;;
  esac
  shift
done

export SHOWCASE_AUTO SHOWCASE_DRY_RUN

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
cleanup() {
  # Only touch port-forwards we started; do not tear down the cluster.
  kill_all_pf 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Preflight (skipped in dry-run so you can rehearse without a cluster)
# ---------------------------------------------------------------------------
if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
  require_cluster
  discover_cluster_state
  start_pf runner-0 "$RUNNER0_PORT"
  start_pf runner-1 "$RUNNER1_PORT"
fi

# ---------------------------------------------------------------------------
# Opening
# ---------------------------------------------------------------------------
opening() {
  title "Agentium OS — Live Demonstration"

  cat <<EOF
  ${CYAN}${BOLD}What you are about to see${NC}

      Five acts, each demonstrating one property of Agentium OS that the
      rest of the agent-platform market does not offer today:

        ${BOLD}1.${NC}  The cluster placement table IS the service mesh
        ${BOLD}2.${NC}  The router refuses unsafe requests on the agent's behalf
        ${BOLD}3.${NC}  The audit trail outlives the pod
        ${BOLD}4.${NC}  Agents move pods; conversation state is the last frontier
        ${BOLD}5.${NC}  Dead runners exit routing in seconds, not kubelet cycles

  ${CYAN}${BOLD}How to watch this${NC}

      Every claim is backed by a real command run against a real k3d
      cluster. There are no slides, no animations, no pre-recorded output.
      Whatever prints on screen is what the cluster actually returned.

      The same five demonstrations are CI-gated test assertions in
      ${DIM}scripts/e2e-k8s/run.sh${NC} — the demo is the test.

EOF
}

closing() {
  title "What you just saw"

  cat <<EOF
  ${BOLD}1. Zero service mesh configuration.${NC}
     The placement table IS the mesh. Deploy anywhere, cluster routes.

  ${BOLD}2. Host-governed safety.${NC}
     SSRF, credential exfiltration, unauthorised control plane — the
     router refuses them before agent code sees the request.

  ${BOLD}3. Shared provenance graph.${NC}
     Audit trail bound to the agent, not the pod. Migrate, crash,
     restart — the history survives.

  ${BOLD}4. Portable agents (frontier: portable conversations).${NC}
     Migrate mid-session and the agent's code, placement, identity, and
     provenance follow. Mid-turn conversation state is the last gap —
     documented, planned. Everyone else is missing the whole list.

  ${BOLD}5. Application-aware health.${NC}
     A dead runner exits routing the moment its heartbeat goes stale —
     ahead of any kubelet signal. Zero requests routed to a corpse.

  ${CYAN}${BOLD}The thesis${NC}

      The winning agent product is not a chat wrapper around a model.
      It's a host that can ${BOLD}run${NC}, ${BOLD}move${NC}, and ${BOLD}explain${NC} delegated work.

  ${CYAN}${BOLD}Run it yourself${NC}

      ${DIM}\$${NC} ./scripts/e2e-k8s/run.sh

      The 15 scenarios in that harness — including the 5 you just saw —
      are CI-gated. Anything on screen here, anyone on your team can
      verify on a laptop in six minutes.

EOF
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

run_single_act() {
  case "$1" in
    1) act_01_mesh ;;
    2) act_02_safety ;;
    3) act_03_provenance ;;
    4) act_04_continuity ;;
    5) act_05_selfheal ;;
    *) die "unknown act '$1' — expected 1..5" ;;
  esac
}

if [[ -n "$SINGLE_ACT" ]]; then
  run_single_act "$SINGLE_ACT"
  exit 0
fi

opening
pause "Act 1 — the service mesh"

act_01_mesh
pause "Act 2 — host-governed safety"

act_02_safety
pause "Act 3 — the audit trail outlives the pod"

act_03_provenance
pause "Act 4 — the migration frontier"

act_04_continuity
pause "Act 5 — self-healing cluster"

act_05_selfheal
pause "the closing"

closing
