#!/usr/bin/env bash
# Verify markdown doc links resolve to existing files (repo-relative paths only).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail=0

check_file() {
  local file="$1"
  while IFS= read -r link; do
    target="${link%%#*}"
    [[ -z "$target" ]] && continue
    [[ "$target" =~ ^https?:// ]] && continue
    [[ "$target" =~ ^mailto: ]] && continue

    local base resolved root_resolved
    base="$(dirname "$file")"
    resolved="$(python3 -c "import os; print(os.path.normpath(os.path.join('$base', '''$target''')))")"

    if [[ ! -e "$resolved" ]]; then
      root_resolved="$(python3 -c "import os; print(os.path.normpath(os.path.join('$ROOT', '''$target''')))")"
      if [[ -e "$root_resolved" ]]; then
        resolved="$root_resolved"
      fi
    fi

    if [[ ! -e "$resolved" ]]; then
      echo "BROKEN: $file -> $link (resolved: $resolved)"
      fail=1
    fi
  done < <(rg -o '\[[^]]*\]\(([^)]+)\)' "$file" -r '$1' --no-line-number 2>/dev/null || true)
}

while IFS= read -r -d '' file; do
  [[ "$file" == *"/.cursor/plans/"* ]] && continue
  check_file "$file"
done < <(
  find . \
    \( -path './docs' -o -path './deploy' -o -path './observability' -o -path './.cursor/agents' -o -path './.cursor/skills' -o -path './crates' -o -maxdepth 1 -name '*.md' -o -path './web/README.md' -o -path './examples' \) \
    -name '*.md' \
    -not -path './target/*' \
    -not -path '*/node_modules/*' \
    -print0
)

if [[ "$fail" -ne 0 ]]; then
  echo "Doc link check failed."
  exit 1
fi

echo "Doc link check passed."
