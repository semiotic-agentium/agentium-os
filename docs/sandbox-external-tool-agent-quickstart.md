# Sandboxed External Tool + Agent Quickstart (Bind rootfs)

Practical, integration-focused checklist for creating:
1) a sandboxed external tool, and
2) an agent that uses it.

For deep runtime/security details, see:
- `docs/host-tool-guide.md`
- `docs/sdk-cli.md`

---

## 0) Prerequisites

- Rust/Cargo
- Docker (only if you use Docker-assisted bind flow)
- Runner built with sandbox support (`microsandbox`)

---

## 1) Scaffold a new sandboxed external tool

Example (Python + sandbox bind runtime):

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

This emits, among others:
- `tool-metadata.json`
- `main.py`
- `tool-server`
- `adapter/Dockerfile`
- `adapter/tool-adapter`
- `setup_bind_sandbox.sh`

> If you skip `--generate-docker`, you can still use bind mode. You just need to materialize rootfs yourself and run `sandbox-bind-sync` (step 3).

---

## 2) Implement tool logic + schema

Update `examples/external-tools/meteo-tool/tool-metadata.json` schema/name as needed:
- `name` (e.g. `dev/meteo`) must match the agent tool allowlist exactly
- keep sandbox runtime shape (`runtime.kind = sandbox`, bind image)

Implement `main.py` handlers for:
- `tool/describe`
- `tool/invoke`

Local probe:

```bash
printf '{"jsonrpc":"2.0","id":1,"method":"tool/describe","params":{"tool_name":"dev/meteo"}}\n' | examples/external-tools/meteo-tool/tool-server
```

---

## 3) Materialize bind rootfs and sync metadata

### Option A: Docker-assisted (recommended for scaffolded bind + docker)

```bash
cargo run -p cargo-agent-platform -- sandbox-bind-sync \
  --tool-dir examples/external-tools/meteo-tool \
  --rootfs .tmp/dev-meteo-rootfs \
  --dockerfile adapter/Dockerfile \
  --image dev-meteo-sandbox:local \
  --force \
  --check
```

### Option B: Existing rootfs (non-Docker pipeline)

```bash
cargo run -p cargo-agent-platform -- sandbox-bind-sync \
  --tool-dir examples/external-tools/meteo-tool \
  --rootfs /abs/path/to/rootfs \
  --check
```

Notes:
- Relative `--rootfs` / `--dockerfile` paths resolve against `--tool-dir`.
- This command computes digest, patches metadata (`runtime.image.path` + `runtime_digest`), and validates with `check-external-tool` when `--check` is set.

---

## 4) Configure runner env for external tools + sandbox

Use colon-separated tool dirs:

```bash
export BAML_EXTERNAL_TOOLS_DIR="examples/external-tools/meteo-tool"
export BAML_SANDBOX_PROVIDER=microsandbox
export BAML_SANDBOX_BIND_ROOTS="$(pwd)/examples/external-tools/meteo-tool/.tmp"
```

Then start runner with features needed for sandbox.

---

## 5) Scaffold an agent that uses the tool

```bash
cargo run -p cargo-agent-platform -- new-agent meteo-agent \
  --template basic-tools \
  --tools "dev/meteo" \
  --tags "dev,meteo" \
  --description "An agent to handle meteo prompts"
```

If agent already exists, regenerate:

```bash
cargo run -p cargo-agent-platform -- regen meteo-agent
```

---

## 6) Keep generated types in sync after schema changes

If tool input/output schema changes:

```bash
BAML_EXTERNAL_TOOLS_DIR="examples/external-tools/meteo-tool" \
cargo run -p cargo-agent-platform -- regen meteo-agent
```

---

## 7) Publish / deploy / chat

```bash
cargo run -p cargo-agent-platform -- publish --agent-dir agents/meteo-agent
cargo run -p cargo-agent-platform -- deploy --hash <HASH>
cargo run -p cargo-agent-platform -- chat --agent meteo-agent
```

---

## Minimal troubleshooting (integration-focused)

### A) `Tool metadata missing for: dev/meteo`
Cause: agent tool name and metadata `name` mismatch.

Fix: align both exactly (`bundle/local_name`).

### B) `failed to read .../tool-metadata.json`
Cause: `BAML_EXTERNAL_TOOLS_DIR` malformed.

Fix: use `:` separator (no commas), and verify directory layout.

### C) Runtime still behaves old after tool changes
Cause: bind rootfs/digest stale.

Fix: rerun `sandbox-bind-sync` (Docker-assisted with `--force` if applicable), then restart runner.

### D) Agent/tool call hangs
Cause: tool didn’t emit a valid JSON-RPC response.

Fix: verify one request -> one response, stdout for protocol only, logs on stderr.

---

## Keep this doc lean

- CLI flag details belong in `docs/sdk-cli.md`
- Runtime/security deep dives belong in `docs/host-tool-guide.md`
- This page is the fast tool+agent integration path
