# Sandboxed external tool + agent quickstart

Fast integration checklist for a sandboxed external tool and an agent that uses it.
For CLI details see [`../reference/sdk-cli.md`](../reference/sdk-cli.md). For runtime and
security see [`../reference/host-tool-guide.md`](../reference/host-tool-guide.md).

## Prerequisites

- Rust/Cargo
- Docker (optional, for Docker-assisted bind flow)
- Runner built with sandbox support (`microsandbox`)

## 1. Scaffold a sandboxed external tool

```bash
cargo run -p agentium -- new-tool meteo-tool \
  --bundle dev \
  --lang python \
  --access read \
  --runtime sandbox \
  --sandbox-source bind \
  --generate-docker \
  --description "Accurate Weather Forecasts for Any Location" \
  --output examples/external-tools/meteo-tool
```

## 2. Implement tool logic + schema

- Set `name` in `tool-manifest.json` (e.g. `dev/meteo`) to match the agent allowlist exactly
- Implement `tool/describe`, `tool/schema`, and `tool/invoke` in your language source (`main.py`, etc.)
- Keep `tool/describe.schema_digest` aligned with the `content_digest` your `tool/schema` handler returns
- Starters embed a working echo schema in source; edit those constants/handlers rather than expecting a generated `tool-schema.json` (optional author files like meteo's `tool-schema.json` are fine if your tool reads them inside `tool/schema`)

## 3. Materialize bind rootfs

```bash
cargo run -p agentium -- sandbox-bind-sync \
  --tool-dir examples/external-tools/meteo-tool \
  --image dev-meteo-sandbox:local \
  --force \
  --check
```

## 4. Configure runner env

```bash
export BAML_EXTERNAL_TOOLS_DIR="examples/external-tools/meteo-tool"
export BAML_SANDBOX_PROVIDER=microsandbox
export BAML_SANDBOX_BIND_ROOTS="$(pwd)/examples/external-tools/meteo-tool/.tmp"
```

## 5. Scaffold and wire the agent

```bash
cargo run -p agentium -- new-agent meteo-agent \
  --template basic-tools \
  --tools "dev/meteo" \
  --tags "dev,meteo" \
  --description "An agent to handle meteo prompts"
```

After schema changes: `BAML_EXTERNAL_TOOLS_DIR=... cargo run -p agentium -- regen meteo-agent`

## 6. Publish, deploy, chat

```bash
cargo run -p agentium -- publish --agent-dir agents/meteo-agent
cargo run -p agentium -- deploy --hash <HASH>
cargo run -p agentium -- chat --agent meteo-agent
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Tool manifest missing for: dev/meteo` | Align agent allowlist and manifest `name` exactly |
| `failed to read .../tool-manifest.json` | Fix `BAML_EXTERNAL_TOOLS_DIR` (colon-separated, no commas) |
| Stale behavior after tool changes | Re-run `sandbox-bind-sync --force`, restart runner |
| Tool call hangs | Verify one JSON-RPC request → one response; logs on stderr only |
