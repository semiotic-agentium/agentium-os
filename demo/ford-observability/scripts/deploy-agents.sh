#!/usr/bin/env bash
# Enable Grafana MCP, regenerate demo agents against the live runner registry,
# then publish+deploy them into the runner via cargo-agent-platform.
#
# Env knobs:
#   RUNNER_URL         runner base URL (default: http://127.0.0.1:18080)
#   REPOSITORY_URL     repository URL (default: $RUNNER_URL/repository)
#   MCP_CONFIG         mcp-servers.json path (default: ~/.agentium-os/mcp-servers.json)
#   GRAFANA_MCP_ID     MCP server id (default: grafana)
#   RUNNER_TOKEN       optional runner token
#   SKIP_MCP_ENABLE    1 to skip mcp enable
#   SKIP_REGEN         1 to skip regen
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$DEMO_DIR/../.." && pwd)"

RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:18080}"
REPOSITORY_URL="${REPOSITORY_URL:-$RUNNER_URL/repository}"
MCP_CONFIG="${MCP_CONFIG:-$HOME/.agentium-os/mcp-servers.json}"
GRAFANA_MCP_ID="${GRAFANA_MCP_ID:-grafana}"

cli=(cargo run -q -p cargo-agent-platform --)
if command -v cargo-agent-platform >/dev/null 2>&1; then
  cli=(cargo-agent-platform)
fi

agent_args=(
  --path "$DEMO_DIR/agents/observability-coordinator"
  --path "$DEMO_DIR/agents/grafana-investigator"
  --path "$DEMO_DIR/agents/slack-notify"
)

push_agents=(
  "$DEMO_DIR/agents/observability-coordinator"
  "$DEMO_DIR/agents/grafana-investigator"
  "$DEMO_DIR/agents/slack-notify"
)

cd "$REPO_ROOT"

if [[ "${SKIP_MCP_ENABLE:-0}" != "1" ]]; then
  if [[ ! -f "$MCP_CONFIG" ]]; then
    echo "[deploy-agents] missing MCP_CONFIG=$MCP_CONFIG" >&2
    echo "[deploy-agents] set MCP_CONFIG or SKIP_MCP_ENABLE=1" >&2
    exit 1
  fi
  echo "[deploy-agents] enabling MCP server '$GRAFANA_MCP_ID' from $MCP_CONFIG"
  "${cli[@]}" mcp enable "$GRAFANA_MCP_ID" \
    --config "$MCP_CONFIG" \
    --repository-url "$REPOSITORY_URL" \
    --runner-token "${RUNNER_TOKEN:-}" \
    --yes
fi

if [[ "${SKIP_REGEN:-0}" != "1" ]]; then
  echo "[deploy-agents] regenerating demo agents from $REPOSITORY_URL"
  BAML_MCP_REGISTRY_URL="$REPOSITORY_URL" \
    "${cli[@]}" regen "${agent_args[@]}"
fi

echo "[deploy-agents] publishing and deploying demo agents to $RUNNER_URL"
"${cli[@]}" push \
  --repository-url "$REPOSITORY_URL" \
  --url "$RUNNER_URL" \
  --runner-token "${RUNNER_TOKEN:-}" \
  --agents "${push_agents[@]}"

echo "[deploy-agents] done"
