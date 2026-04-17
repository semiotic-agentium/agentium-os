# Agent Runner

`baml-agent-runner` is the A2A host: QuickJS + BAML, optional HTTP and/or stdio, embedded agent repository, and deploy-by-hash. **Positional package paths are not accepted** — add agents with `baml-agent-builder publish` (which POSTs to `/repository/publish` and can `POST /deploy`) or call `POST /deploy` yourself.

It supports:

- HTTP and/or stdio serving
- local repository-backed publish/deploy/undeploy by content hash
- startup restore of previously deployed hashes from `--state-dir`
- optional file-backed provenance storage

## Build

```bash
cargo build -p baml-agent-runner --all-features --release
```

For local development, `--all-features` is usually safest to avoid missing tool bundle features.

## Run

```bash
./target/release/baml-agent-runner [flags]
```

Example (empty registry at start; deploy after publish):

```bash
./target/release/baml-agent-runner \
  --serve-http 127.0.0.1:18080 \
  --repository-url http://127.0.0.1:18080/repository \
  --state-dir ./.runner-state \
  --repository-dir ./.repository \
  --provenance-db provenance.db
```

## CLI Options

| Option | Default | Description |
|---|---|---|
| `--serve-http <ADDR>` | unset | Bind HTTP API (example `127.0.0.1:18080`) |
| `--a2a-stdio` | `false` | Run A2A JSON-RPC loop over stdio |
| `--repository-url <URL>` | `http://127.0.0.1:18080/repository` | Base URL used for hash-based deploy/restore lookups |
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

## Authentication and Access Tiers

The runner has two operating modes: **standalone** and **cluster**.

### Standalone mode (default)

All routes are accessible without authentication. This is appropriate for local development and single-runner setups.

### Cluster mode

When `RUNNER_TOKEN` is set (via environment variable or K8s secret), the runner enforces an operator authentication boundary using the `X-Runner-Token` header.

Routes are divided into three tiers:

**Public (no auth required):**

| Route | Description |
|---|---|
| `GET /agents` | Agent discovery |
| `POST /agents/{pkg}/{inst}/a2a` | A2A JSON-RPC |
| `POST /agents/{pkg}/{inst}/a2a/sse` | A2A SSE stream |
| `POST /agents/{pkg}/{inst}/dispatch` | Event delivery |
| `GET /healthz` | Health check |
| `GET /readyz` | Readiness check |
| `GET /openapi.json` | OpenAPI spec |
| `GET /repository/agents` | Repository agent listing |
| `GET /repository/entries` | Repository entry listing |
| `GET /repository/entries/{hash}` | Entry by hash |
| `GET /repository/entries/{name}/{version}` | Entry by name/version |
| `GET /repository/agents/{name}/versions` | Agent version listing |
| `POST /repository/search` | Repository search |
| `GET /repository/lineage/{hash}` | Lineage subgraph |
| `GET /repository/blobs/{hash}` | Artifact download |
| `GET /contexts/...` | Provenance reads |

**Operator-authenticated (require `X-Runner-Token`):**

| Route | Description |
|---|---|
| `GET /config`, `GET /config/*` | Config reads |
| `PUT /config/{bundle_name}` | Config writes |
| `DELETE /config/{bundle_name}` | Config deletion |
| `GET /config/secrets-overview` | Secrets overview |
| `PUT /config/secrets/{name}` | Secret linking |
| `DELETE /config/secrets/{name}` | Secret unlinking |
| `POST /deploy` | Deploy agent by hash |
| `POST /undeploy` | Undeploy agent |
| `GET /deployments` | List deployments |
| `POST /control/migrate` | Agent migration |
| `POST /repository/publish` | Publish agent |
| `POST /repository/fork` | Fork entry |
| `POST /repository/entries/{hash}/tags` | Add tag |
| `DELETE /repository/entries/{hash}/tags` | Remove tag |

**Cluster-internal (runner-to-runner only):**

Cross-runner A2A forwarding uses the same `/agents/.../a2a` routes but relies on K8s NetworkPolicy for isolation. The forwarding path is unauthenticated at the application layer; network isolation is the boundary.

## Deploy vs Publish

- Publish sends source bundle to `/repository/publish`; server-side build stores artifact bytes under canonical `content_hash`.
- Deploy activates a previously published `content_hash` into the running registry.

Typical flow:

1. Start runner with `--serve-http` and repository flags (see above).
2. `baml-agent-builder publish --agent-dir ... --repository-url http://127.0.0.1:18080/repository --deploy-url http://127.0.0.1:18080` (or `POST /deploy` with the printed `content_hash`).

## End-to-End Example

### Standalone (local development)

Start runner:

```bash
./target/debug/baml-agent-runner \
  --a2a-stdio \
  --serve-http 127.0.0.1:18080 \
  --repository-url http://127.0.0.1:18080/repository \
  --state-dir ./.runner-state \
  --repository-dir ./.repository \
  --provenance-db provenance.db
```

Publish source:

```bash
baml-agent-builder publish \
  --agent-dir agents/clickup-agent \
  --repository-url http://127.0.0.1:18080/repository \
  --deploy-url http://127.0.0.1:18080
```

Deploy by content hash (the hash printed in publish result):

```bash
curl -sS -X POST http://127.0.0.1:18080/deploy \
  -H 'content-type: application/json' \
  -d '{"hash":"bfe72df219673c1a919817b29c37c2b51419e1e81b61eeca5e5549bd7b1b5d83"}' | jq
```

### Cluster mode (K8s)

In cluster mode, operator actions require `X-Runner-Token`. The token is provisioned as a K8s secret (see `deploy/demo/run-demo.sh`).

Publish source (operator action):

```bash
curl -sS -X POST http://localhost:18080/repository/publish \
  -H 'X-Runner-Token: <token>' \
  -H 'content-type: application/json' \
  -d @publish-payload.json | jq
```

Deploy by content hash (operator action):

```bash
curl -sS -X POST http://localhost:18080/deploy \
  -H 'X-Runner-Token: <token>' \
  -H 'content-type: application/json' \
  -d '{"hash":"bfe72df219673c1a919817b29c37c2b51419e1e81b61eeca5e5549bd7b1b5d83"}' | jq
```

Discover routing key (public, no auth):

```bash
curl -sS http://localhost:18080/agents \
  | jq '.[].agent_card | {agent_package, agent_instance_id, name}'
```

Expected shape:

```json
{
  "agent_package": "clickup-agent",
  "agent_instance_id": "default",
  "name": "clickup-agent"
}
```

Send prompt (public, no auth):

```bash
curl -sS -X POST "http://localhost:18080/agents/clickup-agent/default/a2a" \
  -H 'content-type: application/json' \
  -d '{
    "jsonrpc":"2.0",
    "id":"1",
    "method":"message.sendStream",
    "params":{
      "message":{
        "messageId":"msg-1",
        "contextId":"ctx-cli-1",
        "role":"user",
        "parts":[{"text":"hello"}]
      }
    }
  }' | jq
```

Important: `message.send` is rejected. Use `message.sendStream`.
Typical error if wrong:

```json
{
  "error": {
    "message": "Invalid request",
    "data": {
      "details": "Only message.sendStream is supported"
    }
  }
}
```

Stream mode (SSE):

```bash
curl -N -X POST "http://localhost:18080/agents/clickup-agent/default/a2a/sse" \
  -H 'content-type: application/json' \
  -d '{
    "jsonrpc":"2.0",
    "id":"2",
    "method":"message.sendStream",
    "params":{"message":{"messageId":"msg-2","contextId":"ctx-cli-1","role":"user","parts":[{"text":"list my pending tasks"}]}}
  }'
```

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
