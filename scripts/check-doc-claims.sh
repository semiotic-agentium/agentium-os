#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Fail when docs/ contain known-stale factual patterns (content grounding guard).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail=0

check_pattern() {
  local label="$1"
  local pattern="$2"
  local hits
  hits="$(rg -n "$pattern" docs/ 2>/dev/null || true)"
  if [[ -n "$hits" ]]; then
    echo "STALE ($label):"
    echo "$hits"
    fail=1
  fi
}

# Stale A2A URL paths (allow prose that says the route does not exist).
while IFS= read -r line; do
  if [[ "$line" =~ no\ separate\ \`/a2a/sse\` ]]; then
    continue
  fi
  if [[ "$line" =~ not\ \`/a2a/sse\` ]]; then
    continue
  fi
  echo "STALE (a2a/sse URL path): $line"
  fail=1
done < <(rg -n '/a2a/sse' docs/ 2>/dev/null || true)

check_pattern "failed_restore|failed_deploy status enum" 'failed_restore|failed_deploy'
check_pattern "phantom bus.emit_envelope metrics" 'bus\.emit_envelope'
check_pattern "foreign credit-onramp examples" 'credit-onramp'

if rg -n 'credit-accounting' docs/reference docs/runbooks 2>/dev/null; then
  echo "STALE (foreign credit-accounting examples in reference/runbooks)"
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo "Doc claim check failed."
  exit 1
fi

echo "Doc claim check passed."
