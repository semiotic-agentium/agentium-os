# meteo-mcp

Open-Meteo weather forecasts exposed as a Model Context Protocol (MCP) server
over stdio. Used by the `meteo-mcp-agent` example to exercise the full PR1–PR5
MCP path: import → approve → builder catalog → runtime resolve → live tool call.

This is the MCP counterpart of the `dev/meteo-tool` external tool under
`examples/external-tools/meteo-tool/`. The two share the same input schema and
the same Open-Meteo HTTP backend; only the wire protocol differs.

## Tool surface

| Field | Value |
|-------|-------|
| Server id | `meteo` |
| Protocol version | `2025-06-18` |
| Tool name (MCP) | `get_meteo` |
| Platform tool name | `mcp/meteo/get_meteo` |
| BAML class name | `McpMeteoGetMeteo` (per `ToolFunctionMetadata::derive_class_name`) |
| Input schema | Identical to `dev/meteo-tool` input (`location_query`, `city`, `country`, `timezone`, `hourly_limit`) |
| Output | MCP `CallToolResult` with summary text block + JSON text block + `structuredContent` |

## Manual smoke test

```bash
printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
| python3 main.py
```

`tools/call` requires network access to `geocoding-api.open-meteo.com` and
`api.open-meteo.com`.

## Operator config

The runner reads `$HOME/.agentium-os/mcp-servers.json`. Render the
in-tree template (substituting the repo root):

```bash
ROOT_DIR=$(git rev-parse --show-toplevel)
mkdir -p ~/.agentium-os
sed "s|\\${ROOT_DIR}|${ROOT_DIR}|g" \
  examples/external-mcp-servers/meteo-mcp/mcp-servers.json.tmpl \
  > ~/.agentium-os/mcp-servers.json
```

Then approve the import into the repository registry:

```bash
cargo agent-platform mcp enable meteo \
  --config ~/.agentium-os/mcp-servers.json \
  --repository-url http://127.0.0.1:18080/repository \
  --yes
```

This creates an immutable approved MCP registry snapshot for `meteo` and its
`mcp/meteo/get_meteo` platform tool entry.

## Where this fits

- Snapshot types and the fake stdio fixture model MCP server/tool identity.
- `cargo agent-platform mcp enable` imports an approved registry snapshot.
- The builder projects registry snapshots into the builder catalog (so agents
  can declare `mcp/meteo/get_meteo` in `manifest.json`).
- The runtime resolver binds package MCP artifacts to a live MCP
  `RunningService`.
- Drift detection marks snapshots stale without mutating compiled BAML schemas.

The `meteo-mcp-agent` example consumes this server end to end.

## Drift demo

To exercise PR5 push-drift, edit `main.py` to change `INPUT_SCHEMA`, restart
the server, and call the tool — the runner will surface
`ConnectionError::Stale` on the next call after `notifications/tools/list_changed`.
(`main.py` does not emit that notification today; see the fake fixture under
`crates/tools/mcp/src/fixture.rs` for a server that does.)

When drift is detected, re-import and approve a new registry snapshot after
reviewing the changed server/tool schemas.
