# Agent Runner

`baml-agent-runner` loads packaged agents (`.tar.gz`) and runs them as an A2A host.
It supports:

- package boot from CLI arguments
- HTTP and/or stdio serving
- local repository-backed deploy/undeploy by content hash
- startup restore of previously deployed hashes
- optional file-backed provenance storage

## Build

```bash
cargo build -p baml-agent-runner --all-features --release
```

For local development, `--all-features` is usually safest to avoid missing tool bundle features.

## Run

```bash
./target/release/baml-agent-runner <agent1.tar.gz> [<agent2.tar.gz> ...] [flags]
```

Example:

```bash
./target/release/baml-agent-runner \
  ./dist/clickup-agent-1.0.0.tar.gz \
  --serve-http 127.0.0.1:8080 \
  --provenance-db provenance.db
```

## CLI Options

| Option | Default | Description |
|---|---|---|
| `<AGENT_PACKAGE>...` | none | Package tarballs to boot at startup (optional; runner can start empty) |
| `--serve-http <ADDR>` | unset | Bind HTTP API (example `127.0.0.1:8080`) |
| `--a2a-stdio` | `false` | Run A2A JSON-RPC loop over stdio |
| `--repository-url <URL>` | `http://127.0.0.1:8080/repository` | Base URL used for hash-based deploy/restore lookups |
| `--repository-dir <DIR>` | `./.repository` | Local repository storage directory used by mounted `/repository` routes |
| `--state-dir <DIR>` | `./.runner-state` | Runner-local deployment state DB directory |
| `--provenance-db <PATH>` | `:memory:` | Provenance storage (`:memory:` or file-backed path) |
| `--web-dir <DIR>` | unset | Serve static web assets from this directory at `/` |
| `--claude-workspaces-base <DIR>` | unset | Workspace root for claude/dev sessions |
| `--stream-idle-secs <SECS>` | `900` | Idle timeout for streaming tool sessions |
| `--invoke <AGENT> <FUNCTION> <JSON_ARGS>` | unset | One-shot invoke mode for a JS function |

## HTTP Surface

When `--serve-http` is set, the runner exposes:

- discovery + A2A routes under `/agents/...`
- deployment lifecycle routes:
  - `POST /deploy`
  - `POST /undeploy`
  - `GET /deployments`
- OpenAPI at `GET /openapi.json`
- repository routes mounted at `/repository` (backed by `--repository-dir`)

For exact request/response DTOs, use `openapi.json`.

## Deploy vs Publish

- Publish stores package blobs + repository metadata in `/repository`.
- Deploy activates a previously published package hash into the running registry.

Typical flow:

1. `cargo agent-platform build`
2. `cargo agent-platform publish --package ...`
3. `POST /deploy` with `hash` or `name+version`

## Startup Restore

On boot, runner:

1. opens deployment state from `--state-dir`
2. reads prior deployment records
3. attempts redeploy for each hash
4. keeps startup running even if some restores fail
5. updates per-deployment failure fields (`last_error`, `last_attempt_at`, `failure_count`)

## A2A Endpoints

- `POST /agents/{agent_package}/{agent_instance_id}/a2a`
  Collect full A2A stream and return JSON-RPC response.
- `POST /agents/{agent_package}/{agent_instance_id}/a2a/sse`
  Stream A2A responses over SSE.

## SSE Runtime Requirement

SSE stream tasks must run on the same long-lived Tokio runtime as HTTP serving.
Do not spawn short-lived nested runtimes for A2A stream handlers, or clients may
see only keep-alive events.
