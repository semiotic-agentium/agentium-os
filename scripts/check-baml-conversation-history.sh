#!/usr/bin/env bash
# Check BAML that uses ctx.tags['conversation_history']: on loop values
# `message` and `msg`, only .role, .content, and .citations are allowed
# (matches wire shape in baml-rt-conversation-spec.md).
#
# BAML_HISTORY_WARN_ONELINE=1 — print stderr warnings for one-line
#   {{ x.role }}: {{ x.content }}
# (off by default; set to 1 in development to find drift)
# BAML_HISTORY_STRICT_ONELINE=1 — treat one-line as failure (exit 1)
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

check_files=()
for f in "${baml_files[@]}"; do
  if [[ -f "$f" ]] && rg -q "ctx\.tags\['conversation_history'\]" "$f" 2>/dev/null; then
    check_files+=("$f")
  fi
done

if [[ ${#check_files[@]} -eq 0 ]]; then
  exit 0
fi

err=0
warn_oneline=0
if [[ -n "${BAML_HISTORY_WARN_ONELINE:-}" && "${BAML_HISTORY_WARN_ONELINE}" == "1" ]]; then
  warn_oneline=1
fi
strict_oneline=0
[[ -n "${BAML_HISTORY_STRICT_ONELINE:-}" && "${BAML_HISTORY_STRICT_ONELINE}" == "1" ]] && strict_oneline=1

for f in "${check_files[@]}"; do
  # Only Jinja lines (avoids @description / comment prose, e.g. "message.thread_ts" as help text).
  while IFS= read -r jline; do
    if [[ ! "$jline" =~ \{\{ ]] && [[ ! "$jline" =~ \{% ]]; then
      continue
    fi
    if [[ ! "$jline" =~ (message|msg)\.[a-zA-Z_][a-zA-Z0-9_]* ]]; then
      continue
    fi
    while IFS= read -r m; do
      [[ -z "$m" ]] && continue
      p=${m#*.}
      case "$p" in
      role|content|citations) ;;
      *)
        echo "$f: disallowed property on history row: $m" >&2
        err=1
        ;;
      esac
    done < <(echo "$jline" | rg -oN '(\bmessage|\bmsg)\.([a-zA-Z_][a-zA-Z0-9_]*)' 2>/dev/null || true)
  done < <(rg -N '(\bmessage|\bmsg)\.([a-zA-Z_][a-zA-Z0-9_]*)' "$f" 2>/dev/null || true)
done

# One-line: role and content in same jinja line (discouraged)
if [[ $warn_oneline -eq 1 || $strict_oneline -eq 1 ]]; then
  for f in "${check_files[@]}"; do
    while IFS= read -r rline; do
      [[ -z "$rline" ]] && continue
      if [[ $strict_oneline -eq 1 ]]; then
        echo "one-line role+content: $f: $rline" >&2
        err=1
      else
        echo "warning: $f: $rline (one-line role+content; prefer BAML_CONVERSATION_HISTORY_JINJA_BLOCK in prompt_copy.rs)" >&2
      fi
    # Escape `{` in regex: `\{\{ ... \.content` (BAML jinja: same-line role and content)
    done < <(rg -n '\}\}:\s*\{\{\s*(msg|message)\.content' "$f" 2>/dev/null || true)
  done
fi

exit "$err"
