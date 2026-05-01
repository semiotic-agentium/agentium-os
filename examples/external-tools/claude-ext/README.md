# dev/claude-ext external tool

This scaffold implements the Agent Platform external tool protocol (JSON-RPC over stdio).

## Wire contract

- Read one JSON-RPC request from stdin.
- Write one JSON-RPC response to stdout.
- Write logs/diagnostics to stderr only.
- Exit after one request.

Supported methods in the scaffold:

- `tool/describe`
- `tool/schema`
- `tool/session_open`
- `tool/session_send`
- `tool/session_read`
- `tool/session_finish`
- `tool/session_abort`

## Local setup

```bash
# Debug build (fast iteration):
cargo run --quiet --manifest-path ./Cargo.toml -- </dev/null || true

# Release build (faster tool invoke after first agent boot):
cargo build --release --manifest-path ./Cargo.toml

# Phase B (real SDK path):
cargo build --release --manifest-path ./Cargo.toml --features sdk-engine
```

Current SDK defaults in `src/claude.rs`:
- tools allowlist: `Read, Write, Edit, LS, Glob, Grep, Bash`
- permission mode: `BypassPermissions`
- auth source: `ANTHROPIC_API_KEY` from sandbox process environment (required)
- session payload secrets are not used for auth in this tool

## Local probe (developer convenience)

For sandbox runtime tools, the runner invokes `/tool-adapter` inside the sandbox.
`tool-server` is **not** the runtime invoke path; it's only a local debugging helper.

```bash
# open
printf '{"jsonrpc":"2.0","id":1,"method":"tool/session_open","params":{"invocation_id":"demo","tool_name":"dev/claude-ext","open_input":{}}}\n' | ./tool-server

# send input (replace demo-session with the session_id returned by session_open)
printf '{"jsonrpc":"2.0","id":2,"method":"tool/session_send","params":{"session_id":"demo-session","input":{"message":"hello"}}}\n' | ./tool-server

# read next step (payloadless, same session_id)
printf '{"jsonrpc":"2.0","id":3,"method":"tool/session_read","params":{"session_id":"demo-session"}}\n' | ./tool-server

# finish (same session_id)
printf '{"jsonrpc":"2.0","id":4,"method":"tool/session_finish","params":{"session_id":"demo-session"}}\n' | ./tool-server
```


## Using with runner dev mode

Set `BAML_EXTERNAL_TOOLS_DIR` to this tool directory (or a colon-separated list), then run your runner:

```bash
export BAML_EXTERNAL_TOOLS_DIR="$(pwd)"
```

Then reference this tool in an agent manifest as:

```json
{ "name": "dev/claude-ext", "backend": "external" }
```

## Bind sandbox runtime notes

These notes apply to both bind modes:

- metadata starts with a placeholder bind path and placeholder digest,
- real `runtime.image.path` + `runtime_digest` must be patched after rootfs exists,
- `check-external-tool` should pass before running the runner,
- sandbox adapters should support TSRPC-framed JSON-RPC for parity with sandbox execution.

## Bind setup (Docker-assisted)

Phase B' note: the sandbox image now bundles Node.js + pinned `@anthropic-ai/claude-code` CLI.
The Docker build runs `claude --version` as a build-time self-check.

Helper scripts are scaffolded at:

- `./setup_bind_sandbox.sh` (build/export/sync/check)
- `./inspect_tool.py` (framed adapter probe: describe/schema/invoke)

```bash
./setup_bind_sandbox.sh --force

# Optional: override pinned Claude CLI version at build time
# (default in adapter/Dockerfile: 2.1.121)
# docker build -f adapter/Dockerfile --build-arg CLAUDE_CODE_NPM_VERSION=2.1.121 .
```

This script wraps `sandbox-bind-sync` to build `adapter/Dockerfile`, export bind
rootfs, patch metadata, materialize adapter sidecar bundle (`/etc/agentium/tool-bundle.json`),
and run `check-external-tool`.

> Bind rootfs mode copies filesystem contents only. Docker image config
> (like `ENV TOOL_CMD=...`) is not guaranteed at runtime. The generated
> adapter resolves a default tool command without requiring env vars.

You can also call the command directly:

```bash
cargo run -q -p cargo-agent-platform -- sandbox-bind-sync \
  --tool-dir . \
  --rootfs ./.tmp/bind-rootfs \
  --dockerfile adapter/Dockerfile \
  --image local-sandbox:latest \
  --force \
  --check
```

`adapter/tool-adapter` is a generated transport shim (TSRPC <-> raw stdio).
You usually only edit the scaffolded language source (`main.py`, `src/main.rs`, etc.).

Example probes:

```bash
# Reads bind artifact sidecar from .tmp/<tool>-rootfs by default.
./inspect_tool.py describe
./inspect_tool.py schema

# invoke requires a runnable adapter command
./inspect_tool.py invoke --input '{"message":"hello"}' -- docker run --rm -i local-sandbox:latest /tool-adapter
```

