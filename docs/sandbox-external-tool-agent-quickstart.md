# Sandboxed External Tool + Agent Quickstart (Bind rootfs)

This is a practical checklist for creating:
1) a sandboxed external tool, and
2) an agent that uses it.

It is based on the `dev/meteo` flow used in this repo.

---

## 0) Prerequisites

- Rust/Cargo
- Docker
- `jq`
- Runner with sandbox support (`microsandbox`)

---

## 1) Scaffold a new external tool

Example (Python + sandbox bind runtime):

```bash
cargo run -p cargo-agent-platform -- new-tool meteo-tool \
  --bundle dev \
  --lang python \
  --access read \
  --runtime sandbox \
  --sandbox-source bind \
  --description "Accurate Weather Forecasts for Any Location" \
  --output examples/external-tools/meteo-tool
```

This creates:
- `tool-metadata.json`
- `main.py`
- `tool-server`
- `README.md`
- etc.

If you want scaffolded Docker artifacts too, add `--generate-docker`.
That also emits `adapter/Dockerfile` + `adapter/tool-adapter` +
`setup_bind_sandbox.sh` (Docker build/export + digest + patch + validate).

---

## 2) Implement tool logic + schema

Update `examples/external-tools/meteo-tool/tool-metadata.json`:
- `name` (e.g. `dev/meteo`) **must match agent tool name exactly**.
- `runtime.kind = sandbox`
- `runtime.image.kind = bind`
- replace scaffold placeholder `runtime.image.path = "<rootfs-path>"` with your real bind rootfs directory
- replace scaffold placeholder `runtime_digest = sha256:00..` with a recomputed digest
- `runtime.entrypoint = ["/tool-adapter"]`

Implement protocol handling in `main.py`:
- handle `tool/describe`
- handle `tool/invoke`
- return JSON-RPC errors for invalid input (`-32602`) instead of crashing

Sandbox protocol invariant (including Bind rootfs):
- `/tool-adapter` must speak **TSRPC-framed JSON-RPC** on stdin/stdout
- framing = 4-byte big-endian payload length + JSON body
- raw newline JSON-RPC is fine for `tool-server` local probes, but not sufficient for sandbox adapter execution

Validate metadata:

```bash
cargo run -q -p cargo-agent-platform -- check-external-tool --path examples/external-tools/meteo-tool
```

---

## 3) Build sandbox image + export bind rootfs

Create adapter Dockerfile (example path):
- `examples/external-tools/meteo-tool/adapter/Dockerfile`
- must expose `/tool-adapter` as entrypoint

Build image:

```bash
docker build -t dev-meteo-sandbox:local \
  -f examples/external-tools/meteo-tool/adapter/Dockerfile \
  .
```

Export rootfs + patch digest/entrypoint metadata:

```bash
./examples/external-tools/meteo-tool/setup_bind_demo.sh --image dev-meteo-sandbox:local --force
```

This script should:
- export image filesystem to `.tmp/dev-meteo-rootfs`
- compute `runtime_digest`
- patch `tool-metadata.json` with bind path + entrypoint + digest
- re-run `check-external-tool`

If you hit command-resolution mismatches (e.g., installed `cargo agent-platform`
plugin missing newer subcommands), set:

```bash
export AGENT_PLATFORM_CMD='cargo run -q -p cargo-agent-platform --'
```

before running `setup_bind_sandbox.sh`.

---

## 4) Set env vars for external tool discovery + sandbox

Use **colon-separated** tool dirs (NOT comma-separated):

```bash
export BAML_EXTERNAL_TOOLS_DIR="examples/external-tools/dev_echo_sandbox:examples/external-tools/meteo-tool"
export BAML_SANDBOX_PROVIDER=microsandbox
export BAML_SANDBOX_BIND_ROOTS="$(pwd)/.tmp"
```

Current microsandbox network default:
- `public_only` egress policy (public internet allowed)
- loopback/private/link-local/metadata blocked

If your tool needs internet APIs and still times out, verify you are running the updated runner binary and provider (`BAML_SANDBOX_PROVIDER=microsandbox`).

---

## 5) Scaffold an agent that uses the tool

```bash
cargo run -p cargo-agent-platform -- agent-platform new-agent meteo-agent \
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

If you change external tool input/output schema, run:

```bash
BAML_EXTERNAL_TOOLS_DIR="examples/external-tools/dev_echo_sandbox:examples/external-tools/meteo-tool" \
cargo run -p cargo-agent-platform -- regen meteo-agent
```

This updates generated files such as:
- `agents/meteo-agent/baml_src/_baml_runtime.baml`
- `agents/meteo-agent/src/baml-runtime.d.ts`

---

## 7) Publish/deploy/test (optional)

```bash
cargo run -p cargo-agent-platform -- publish --agent-dir agents/meteo-agent
cargo run -p cargo-agent-platform -- deploy --hash <HASH>
cargo run -p cargo-agent-platform -- chat --agent meteo-agent
```

---

## Minimal troubleshooting

### A) `failed to read .../tool-metadata.json`
Cause: `BAML_EXTERNAL_TOOLS_DIR` is malformed.

Fix: use `:` separator, no commas.

---

### B) `Tool metadata missing for: dev/meteo`
Cause: agent tool name and metadata `name` don’t match.

Fix: align both to same exact string (e.g. `dev/meteo`).

---

### C) Regen fails with unsupported JSON schema (`anyOf`/shape errors)
Cause: schema contains forms current generator cannot map.

Fix: simplify schema to supported object typing (explicit `type`, `properties`, `required`).

---

### D) Tool code changed but runtime still behaves old
Cause: bind rootfs/digest is stale.

Fix:
1. rebuild docker image
2. re-export rootfs (`--force`)
3. recompute digest + patch metadata (`setup_bind_demo.sh`)

---

### E) Agent seems to wait forever
Usually happens if tool never writes JSON-RPC response.

Use a TSRPC probe (for example `examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py`)
when your adapter uses compatible input keys:

```bash
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
  --adapter ./.tmp/dev-echo-rootfs/tool-adapter describe
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
  --adapter ./.tmp/dev-echo-rootfs/tool-adapter invoke \
  --tool-name dev/echo \
  --message "hello"
```

Checklist:
- ensure one request -> one response
- `stdout` only for protocol frames
- logs only on `stderr`
- sandbox `/tool-adapter` must use TSRPC-framed JSON-RPC (4-byte BE length + JSON body)
- raw newline JSON-only handling can hang under sandbox invoke
- keep HTTP timeouts in tool code
- catch exceptions and return JSON-RPC error frame

---

## Practical best practices

- Keep tool name stable and explicit (`dev/<tool>`).
- Do validation in tool code and return `invalid_argument` for bad input.
- Prefer tool-side disambiguation/clarification signals for ambiguous entities.
- After schema changes: `check-external-tool` + `regen <agent>`.
- After runtime code changes (sandbox bind): rebuild image + refresh rootfs digest.
