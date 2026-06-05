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
cargo run -p cargo-agent-platform -- new-tool meteo-tool \
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
- Implement `tool/describe` and `tool/invoke` in `main.py`

## 3. Materialize bind rootfs

```bash
cargo run -p cargo-agent-platform -- sandbox-bind-sync \
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
cargo run -p cargo-agent-platform -- new-agent meteo-agent \
  --template basic-tools \
  --tools "dev/meteo" \
  --tags "dev,meteo" \
  --description "An agent to handle meteo prompts"
```

After schema changes: `BAML_EXTERNAL_TOOLS_DIR=... cargo run -p cargo-agent-platform -- regen meteo-agent`

## 6. Publish, deploy, chat

```bash
cargo run -p cargo-agent-platform -- publish --agent-dir agents/meteo-agent
cargo run -p cargo-agent-platform -- deploy --hash <HASH>
cargo run -p cargo-agent-platform -- chat --agent meteo-agent
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Tool manifest missing for: dev/meteo` | Align agent allowlist and manifest `name` exactly |
| `failed to read .../tool-manifest.json` | Fix `BAML_EXTERNAL_TOOLS_DIR` (colon-separated, no commas) |
| Stale behavior after tool changes | Re-run `sandbox-bind-sync --force`, restart runner |
| Tool call hangs | Verify one JSON-RPC request → one response; logs on stderr only |
