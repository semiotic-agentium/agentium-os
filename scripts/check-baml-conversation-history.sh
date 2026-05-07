#!/usr/bin/env bash
# Reject BAML that references ctx.tags conversation_history — canonical history is
# conversation_transcript only (see docs/intent-based-planning-and-session-prompting.md).
#
# Usage: from repo root. No args: scan agents/ and tests/fixtures/agents/ .baml
# Or pass .baml paths (pre-commit).
set -euo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null || true)
if [[ -z "$root" ]]; then
  root="."
fi
cd "$root"

if ! command -v rg &>/dev/null; then
  echo >&2 "check-baml-conversation-history: ripgrep (rg) is required"
  exit 1
fi

baml_files=()
if [[ $# -gt 0 ]]; then
  for a; do
    case "$a" in
    *.baml) baml_files+=("$a") ;;
    esac
  done
else
  while IFS= read -r p; do
    baml_files+=("$p")
  done < <(find agents tests/fixtures/agents -name '*.baml' -type f 2>/dev/null | sort)
fi

if [[ ${#baml_files[@]} -eq 0 ]]; then
  exit 0
fi

bad=0
for f in "${baml_files[@]}"; do
  [[ -f "$f" ]] || continue
  if rg -n "ctx\.tags(\['conversation_history'\]|\.conversation_history)" "$f" >/dev/null 2>&1; then
    echo >&2 "$f: forbidden ctx.tags conversation_history — use conversation_transcript only"
    bad=1
  fi
done

exit "$bad"
