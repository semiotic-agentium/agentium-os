#!/usr/bin/env bash
# Per-file #[test] / #[tokio::test] counts for matrix-consolidation triage.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
cd "$root"

echo "test inventory (sync #[test] + #[tokio::test] in crates/)"
echo "format: COUNT  PATH"
echo "---"

rg -l '#\[(tokio::)?test\]' crates --glob '*.rs' 2>/dev/null | while read -r f; do
  sync=$(rg -c '#\[test\]' "$f" 2>/dev/null || true)
  async=$(rg -c '#\[tokio::test\]' "$f" 2>/dev/null || true)
  sync=${sync:-0}
  async=${async:-0}
  total=$((sync + async))
  if [[ "$total" -gt 0 ]]; then
    printf '%4d  %s\n' "$total" "$f"
  fi
done | sort -t' ' -k1 -nr

echo "---"
total=$(rg '#\[(tokio::)?test\]' crates --glob '*.rs' 2>/dev/null | wc -l | tr -d ' ')
echo "workspace total (line matches): $total"
