# dev/meteo-tool external tool

This scaffold implements the Agent Platform external tool protocol (JSON-RPC over stdio).

## Wire contract

- Read one JSON-RPC request from stdin.
- Write one JSON-RPC response to stdout.
- Write logs/diagnostics to stderr only.
- Exit after one request.

Supported methods in the scaffold:

- `tool/describe`
- `tool/schema`
- `tool/invoke`

## Local setup

```bash
python3 ./main.py </dev/null || true
```

## Local probe (developer convenience)

For sandbox runtime tools, the runner invokes `/tool-adapter` inside the sandbox.
This local source probe only checks the underlying tool logic; it bypasses sandbox framing/rootfs/image checks.

```bash
printf '{"jsonrpc":"2.0","id":1,"method":"tool/describe","params":{"tool_name":"dev/meteo-tool"}}\n' | python3 ./main.py
printf '{"jsonrpc":"2.0","id":2,"method":"tool/invoke","params":{"invocation_id":"demo","tool_name":"dev/meteo-tool","input":{"location_query":"Athens, Greece"}}}\n' | python3 ./main.py
```


## Using with runner dev mode

Set `BAML_EXTERNAL_TOOLS_DIR` to this tool directory (or a colon-separated list), then run your runner:

```bash
export BAML_EXTERNAL_TOOLS_DIR="$(pwd)"
```

Then reference this tool in an agent manifest as:

```json
{ "name": "dev/meteo-tool", "backend": "external" }
```

## Bind sandbox runtime notes

These notes apply to both bind modes:

- metadata uses a portable tool-relative bind path,
- local absolute bind paths live in gitignored `tool-metadata.lock.json`,
- `check-external-tool` should pass before running the runner,
- sandbox adapters should support TSRPC-framed JSON-RPC for parity with sandbox execution.

## Bind setup (Docker-assisted)

A helper script is scaffolded at `./setup_bind_sandbox.sh`.

```bash
./setup_bind_sandbox.sh --force
```

This script wraps `sandbox-bind-sync` to build `adapter/Dockerfile`, export bind
rootfs, write the local lock file, materialize adapter sidecar bundle (`/etc/agentium/tool-bundle.json`),
run `check-external-tool`, and run a framed
`tool/describe` smoke-test against rootfs `/tool-adapter`.

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

Pass `--no-smoke-test` to skip the framed `tool/describe` probe (e.g. when the
adapter binary can't run directly on the host).

`adapter/tool-adapter` is a generated transport shim (TSRPC <-> raw stdio).
You usually only edit the scaffolded language source (`main.py`, `src/main.rs`, etc.).

