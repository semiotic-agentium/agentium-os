#!/usr/bin/env bash
set -euo pipefail

export BAML_SANDBOX_BIND_ROOTS=/home/neithanmo/Documents/Work/Semiotic/agent-platform-notify/examples/external-tools/claude-ext/.tmp/
export BAML_EXTERNAL_TOOLS_DIR="$PWD/examples/external-tools/claude-ext"
export CLAUDE_EXT_USE_SHELL_PIPELINE=0

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

docker image rm dev-claude-ext-sandbox:local || true
rm -rf examples/external-tools/claude-ext/.tmp
./examples/external-tools/claude-ext/setup_bind_sandbox.sh --force

exec cargo run -p baml-agent-runner --all-features -- \
    --a2a-stdio \
    --serve-http 127.0.0.1:18080 \
    --provenance-db provenance.db
