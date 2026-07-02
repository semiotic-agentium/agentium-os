#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0
#
# Publish + deploy this agent into an already-running runner.
# Usage: ./deploy.sh [runner-url]   (default http://127.0.0.1:18087)
set -euo pipefail

RUNNER_URL="${1:-http://127.0.0.1:18087}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
cd "$REPO_ROOT"

echo "Publishing to ${RUNNER_URL}/repository ..."
HASH="$(cargo run -q -p agentium -- publish \
  --agent-dir examples/agents/deploy-health-consumer \
  --repository-url "${RUNNER_URL}/repository" \
  --origin original 2>&1 | sed -n 's/^[[:space:]]*hash:[[:space:]]*//p')"

if [ -z "${HASH}" ]; then
  echo "publish failed: no hash returned" >&2
  exit 1
fi
echo "Published hash: ${HASH}"

echo "Deploying into ${RUNNER_URL} ..."
cargo run -q -p agentium -- deploy \
  --hash "${HASH}" \
  --url "${RUNNER_URL}"
