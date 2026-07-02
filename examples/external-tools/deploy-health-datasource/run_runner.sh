#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0
#
# Start a runner with the raw external-datasource demo activated. The tool dir
# is on the allowed list (runner.toml [external_tools].dirs), so it is
# discovered + auto-approved at boot, then mounted because runner.toml also
# activates it under [external_datasources].
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
cd "$REPO_ROOT"

exec cargo run -p agentium -- serve -- \
  --runner-config examples/external-tools/deploy-health-datasource/runner.toml \
  --serve-http 127.0.0.1:18080 \
  --event-poll-interval-secs 1
