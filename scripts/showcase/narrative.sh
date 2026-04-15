#!/usr/bin/env bash
# Narrative helpers for the Agentium OS live terminal showcase.
# Sourced by demo.sh and individual act scripts — not executed directly.

# ---------------------------------------------------------------------------
# Styling
# ---------------------------------------------------------------------------
# Emit ANSI only when stdout is a TTY, so recorded output stays clean.
if [[ -t 1 ]]; then
  BOLD=$'\033[1m'
  DIM=$'\033[2m'
  CYAN=$'\033[36m'
  GREEN=$'\033[32m'
  YELLOW=$'\033[33m'
  RED=$'\033[31m'
  NC=$'\033[0m'
else
  BOLD=""; DIM=""; CYAN=""; GREEN=""; YELLOW=""; RED=""; NC=""
fi

RULE="━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ---------------------------------------------------------------------------
# Runtime flags (set by demo.sh, honored here)
# ---------------------------------------------------------------------------
SHOWCASE_AUTO=${SHOWCASE_AUTO:-false}      # --auto: skip pauses
SHOWCASE_DRY_RUN=${SHOWCASE_DRY_RUN:-false} # --dry-run: print narration, don't execute

# ---------------------------------------------------------------------------
# Headers
# ---------------------------------------------------------------------------

# title <top-line>
title() {
  echo ""
  echo "${BOLD}${RULE}${NC}"
  echo "${BOLD}  $*${NC}"
  echo "${BOLD}${RULE}${NC}"
  echo ""
}

# act_header <n> <total> <name>
act_header() {
  local n="$1" total="$2" name="$3"
  echo ""
  echo "${BOLD}${RULE}${NC}"
  echo "${BOLD}  ACT ${n} / ${total}  ${DIM}•${NC}${BOLD}  ${name}${NC}"
  echo "${BOLD}${RULE}${NC}"
  echo ""
}

# section <label> — sub-heading inside an act
section() {
  echo ""
  echo "  ${CYAN}${BOLD}$*${NC}"
}

# ---------------------------------------------------------------------------
# Narration
# ---------------------------------------------------------------------------

# claim <text...> — the headline assertion of this act
claim() {
  section "The claim"
  for line in "$@"; do
    echo "      $line"
  done
  echo ""
}

# explain <text...> — inline narration during a step
explain() {
  for line in "$@"; do
    echo "  ${DIM}$line${NC}"
  done
}

# step <label>
step() {
  echo ""
  echo "  ${CYAN}▸${NC} ${BOLD}$*${NC}"
}

# takeaway <text...>
takeaway() {
  section "Takeaway"
  for line in "$@"; do
    echo "      $line"
  done
  echo ""
}

# ---------------------------------------------------------------------------
# Commands and evidence
# ---------------------------------------------------------------------------

# cmd <display-command>
# Just print a command the way a human would type it, for the presenter to
# read aloud before showing its output. Does not execute.
cmd() {
  echo ""
  echo "      ${DIM}\$${NC} $*"
}

# run <command...>
# Execute a command, unless --dry-run. Stderr merges with stdout so rejections
# from the router show up in the captured output.
run() {
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    echo "      ${DIM}(dry-run: skipped)${NC}"
    return 0
  fi
  "$@"
}

# show <output>
# Print captured output with left-margin so it lines up visually with the cmd line.
show() {
  local line
  while IFS= read -r line; do
    echo "      $line"
  done <<<"$1"
}

# result <text...> — a green "what that tells us" annotation after command output
result() {
  for line in "$@"; do
    echo "      ${GREEN}→${NC} $line"
  done
}

# warn <text> — a yellow soft-warning inline (for flake-prone assertions)
warn() {
  echo "      ${YELLOW}!${NC} $*"
}

# fail_soft <text> — red "this did not match expected", does not exit
fail_soft() {
  echo "      ${RED}✗${NC} $*"
}

# ---------------------------------------------------------------------------
# Pause control
# ---------------------------------------------------------------------------

# pause <next-act-preview>
pause() {
  if [[ "$SHOWCASE_AUTO" == "true" || "$SHOWCASE_DRY_RUN" == "true" ]]; then
    echo ""
    return 0
  fi
  echo ""
  if [[ -n "${1:-}" ]]; then
    echo "  ${DIM}(Press Enter for $1)${NC}"
  else
    echo "  ${DIM}(Press Enter to continue)${NC}"
  fi
  read -r _ || true
}

# ---------------------------------------------------------------------------
# Fatal error
# ---------------------------------------------------------------------------
die() {
  echo ""
  echo "${RED}${BOLD}  ✗ ${1}${NC}" >&2
  if [[ $# -gt 1 ]]; then
    shift
    for line in "$@"; do
      echo "    $line" >&2
    done
  fi
  echo ""
  exit 1
}
