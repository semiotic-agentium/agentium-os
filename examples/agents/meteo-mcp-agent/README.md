# meteo-mcp-agent

Mirror of `meteo-agent` that drives the in-tree `meteo-mcp` server instead of
the sandboxed `dev/meteo-tool` external tool. Used to exercise the registry-first
MCP path end to end: registry import, builder catalog projection, runtime
resolver wiring, and live `tools/call` over stdio.

## Prerequisites

1. Start a runner exposing repository routes, then approve the `meteo-mcp`
   server into the MCP registry. The helper script wraps this flow:

   ```bash
   scripts/meteo_mcp.sh runner   # terminal 1
   scripts/meteo_mcp.sh chat     # terminal 2; imports + publishes + chats
   ```

   Manual registry import:

   ```bash
   ROOT_DIR=$(git rev-parse --show-toplevel)
   mkdir -p ~/.agentium-os
   sed "s|\\${ROOT_DIR}|${ROOT_DIR}|g" \
     examples/external-mcp-servers/meteo-mcp/mcp-servers.json.tmpl \
     > ~/.agentium-os/mcp-servers.json

   cargo agent-platform mcp enable meteo \
     --config ~/.agentium-os/mcp-servers.json \
     --repository-url http://127.0.0.1:18080/repository \
     --yes
   ```

   After this, `mcp/meteo/get_meteo` resolves through the builder catalog and
   the runtime resolver using registry-derived package artifacts.

2. `OPENROUTER_API_KEY` set in the runner environment — the agent prompt
   uses `openai/gpt-4o-mini` via OpenRouter, same as `meteo-agent`.

## Build

```bash
BAML_MCP_REGISTRY_URL=http://127.0.0.1:18080/repository \
  baml-agent-builder package \
    --agent-dir examples/agents/meteo-mcp-agent \
    --output /tmp/meteo-mcp-agent.tar.gz
```

The builder regenerates `baml_src/_baml_runtime.baml` and
`src/baml-runtime.d.ts` from the manifest tools — the MCP tool's classes are
projected from the approved registry snapshot (`McpMeteoGetMeteo`,
`McpMeteoGetMeteoInput`, `McpMeteoGetMeteoSessionPlan`, etc.).

## Run

```bash
baml-agent-builder publish \
  --agent-dir examples/agents/meteo-mcp-agent \
  --repository-url http://127.0.0.1:18080/repository \
  --deploy-url http://127.0.0.1:18080 \
  --message "meteo-mcp demo"
```

Then send a chat message to `meteo-mcp-agent/default`. On the first tool
call the runner spawns the Python MCP child process via `TokioChildProcess`
and pools the connection for the agent's lifetime.

## What differs from `meteo-agent`

| | `meteo-agent` | `meteo-mcp-agent` |
|---|---|---|
| Tool name | `dev/meteo-tool` | `mcp/meteo/get_meteo` |
| BAML class | `DevMeteoTool*` | `McpMeteoGetMeteo*` |
| Transport | TSRPC-framed stdio sandbox | MCP line-delimited stdio |
| Output schema validation | Yes (tool-metadata.json) | No — MCP 2025-06-18 has no output schema; runtime returns `ContentEnvelope` (`content[]` + `structuredContent`) |
| Tool registration | Inventory at compile time | MCP registry snapshot projected into package artifacts |

Input schema is byte-for-byte identical so prompt logic transfers without
adjustment.
