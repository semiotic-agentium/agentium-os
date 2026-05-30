#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Pre-commit: when fixture-relevant paths are staged, run regen_fixtures and
# require agents/ + tests/fixtures/agents/ to match (no unstaged drift).
#
# Skip: SKIP=regen-fixtures git commit ...
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

if [[ "${SKIP_REGEN_FIXTURES:-}" == "1" ]] || [[ "${SKIP:-}" == *regen-fixtures* ]]; then
  exit 0
fi

# Pre-commit only invokes this hook when staged files match .pre-commit-config.yaml `files`.
if [[ $# -eq 0 ]]; then
  exit 0
fi

echo "regen-fixtures: staged changes touch builder/tools/agents; running regen_fixtures…"
cargo run -p baml-rt-builder --features http-tools,memory,security-eval --bin regen_fixtures

if ! git diff --exit-code -- agents tests/fixtures/agents; then
  echo >&2
  echo >&2 "regen_fixtures changed generated files under agents/ or tests/fixtures/agents/."
  echo >&2 "Stage the updates and re-commit, e.g.:"
  echo >&2 "  git add agents tests/fixtures/agents"
  echo >&2 "Or skip (not recommended): SKIP=regen-fixtures git commit …"
  exit 1
fi

exit 0
