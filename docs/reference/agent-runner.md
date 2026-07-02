# Agent Runner (HTTP API)

The platform host runs as **`agentium serve`** (library: `baml-agent-runner`). It is the A2A host: QuickJS + BAML, optional HTTP and/or stdio, embedded agent repository, and deploy-by-hash. **Positional package paths are not accepted** — add agents with `agentium install agent` (POST `/repository/publish` + `POST /deploy`) or call `POST /deploy` yourself.

For CLI flags and local dev, see [`agentium-cli.md`](agentium-cli.md).

It supports:

- HTTP and/or stdio serving
- local repository-backed publish/deploy/undeploy by content hash
- startup restore of previously deployed hashes from `--state-dir`
- optional file-backed provenance storage

## Build

```bash
cargo build -p agentium --all-features --release
# or: just build-release
```

For local development, `--all-features` is usually safest to avoid missing tool bundle features.

## Run

```bash
./target/release/agentium serve [flags]
```

Example (empty registry at start; deploy after publish):

```bash
./target/release/agentium serve \
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

## Context compaction (host BAML)

When conversation history exceeds the compaction threshold, the runner summarizes sealed prefix rows via the host-owned BAML function `SummarizeConversationPrefix` (`baml_src/host/context_compaction.baml`). The LLM returns prose only; Rust wraps it in a `[conversation summary]` envelope and **validates** any `#N`/`@N`/`@prefix/local` handles cited in the summary against the conversation ref table hydrated at prepare time. [`invoke_summarizer_with_retry`](../../crates/baml-rt-provenance/src/context_compaction/summarizer.rs) runs up to two attempts per trigger; on validation failure the second attempt passes the validator error as `validation_feedback` in the BAML prompt. If both attempts fail, compaction is skipped for that trigger (`summarize_failed` metric with reason `validation`) and is retried on the next `ContextHistorySettled` event. Covered prefix rows remain in the provenance graph regardless. LLM routing uses the agent's stored `LlmClientConfig` (same resolver as agent BAML hops).

**Triggering** is model-aware and resolved from the `llm` config bundle:

- **Post-turn** (`ContextHistorySettled`): runs after a history mutation cycle completes — A2A chat stream terminal (`InputRequired`, semantic final, channel closed) or host `onDispatch` ack. Evaluates against the **final** transcript for that cycle (not `A2aCompleted` stream handover). Defers mid-turn on `awaiting_input` only; settlement after `InputRequired` is allowed. Pre-model emergency compaction covers prompt overflow when post-turn defers.
- **Pre-model emergency** (`conversation_history_json_for_llm` read): runs when full agent-prompt byte estimate exceeds the resolved model emergency budget for the effective BAML function.
- **Manual operator** (future API): bypasses threshold checks with `force`, but safety gates still apply unless explicitly overridden.

**History kinds:** compaction is history-kind agnostic — chat messages, host ingress operational rows (`role=host`), ingress user rows, planning events, and tool/session rows all contribute to prefix selection and summarizer input. Live in-progress planning steps stay verbatim in the retained tail; sealed intents/plans are summarizable.

**Invariants (I1–I6):** settlement timing, history-kind coverage, planning preservation, prompt boundedness under sustained chat or event intake, pre-model emergency fallback, and R9 append-only graph with compaction on agent read only.

Budget resolution order: client override → model override → known-model table → online metadata cache (OpenRouter) → conservative fallback. Operators configure overrides under **Settings → LLM → Compaction budgets**; resolved budgets are exposed at `GET /config/llm/model-budgets`.

Set `BAML_HOST_SCHEMA_DIR` to the directory that **contains** `baml_src/host/` (typically the repository root). If unset, the runner defaults to the repo root relative to the binary build, or `/opt/agentium` in the published container image. Boot fails fast if the host schema cannot be loaded.

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

The runner has two orthogonal axes: **cluster topology** (shared SurrealDB) and **operator token** gating.

### Cluster topology

**Cluster mode** is enabled when the runner uses a remote shared SurrealDB (`--surreal-endpoint` / `ProvenanceDb::Remote`), not merely when `RUNNER_TOKEN` is set. Cluster mode registers the runner in the shared registry, enables cross-pod A2A forwarding, and adds a **cluster heartbeat** gate to `/readyz`.

**Standalone mode** uses local provenance storage (default `:memory:` or file-backed `--provenance-db` without remote endpoint).

### Operator token (`RUNNER_TOKEN` / `--runner-token`)

When a runner token is configured, **operator-tier routes** require a valid `X-Runner-Token` header in both standalone and cluster mode. When no token is configured:

- **Standalone:** operator routes are open (local dev default).
- **Cluster:** operator routes **fail closed** (401) until a token is configured.

Public routes (`/agents/.../chat` JSON-RPC, `/agents/.../a2a`, `/dispatch`, discovery, conversation history, health probes) remain reachable without the token.

Routes are divided into tiers:

**Public (no auth required):**

| Route | Description |
|---|---|
| `GET /agents` | Agent discovery |
| `POST /agents/{pkg}/{inst}/a2a` | A2A JSON-RPC; streaming methods return `text/event-stream` on this same path |
| `POST /agents/{pkg}/{inst}/dispatch` | Event delivery |
| `GET /healthz` | Liveness (always 200 while process is up) |
| `GET /readyz` | Readiness: boot latch **and** runtime-progress lag **and** cluster heartbeat (when wired) |
| `GET /diagnose` | Runtime diagnostics |
| `GET /openapi.json` | OpenAPI spec |
| `POST /events/publish` | Host event publish (Event Console) |
| `POST /event-dispatch/validate` | Draft validation |
| `GET /message-shapes` | Event Console registry |
| `GET /contexts/*`, `GET /tasks/*/episode*` | Conversation history, provenance, metrics |
| `GET /repository/*` (read paths) | Repository reads when mounted, including `GET /repository/dev-artifacts` |

**Operator-authenticated (require `X-Runner-Token` when token configured; fail-closed in cluster without token):**

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
| `POST /eval/sessions` | Create ephemeral eval model-override session (`X-Agentium-Eval-Session` on subsequent A2A) |
| `POST /control/migrate` | Agent migration |
| `GET /cluster/agents` | Cluster agent listing |
| `POST /cluster/deploy` | Cluster-wide deploy fan-out |
| `POST /repository/publish` | Publish agent |
| `POST /repository/fork` | Fork entry |
| `POST /repository/entries/{hash}/tags` | Add tag |
| `DELETE /repository/entries/{hash}/tags` | Remove tag |

**Note:** There is no separate `/a2a/sse` route. SSE streaming uses `POST .../a2a` with `Accept: text/event-stream` (or JSON-RPC streaming methods on the same handler).

**Cluster-internal A2A forwarding:**

Cross-runner A2A forwarding uses the same `/agents/.../a2a` routes. The forwarding path is unauthenticated at the application layer; the cluster fabric perimeter is the trust boundary. The runner NetworkPolicy does not fence the route — the policy has a permissive any-source rule on the runner port (see [`deploy/helm/agentium-os/templates/networkpolicy.yaml`](../deploy/helm/agentium-os/templates/networkpolicy.yaml)). Operator routes on the same port are protected separately by `X-Runner-Token`.

## Tool Access

A deployed agent's tool surface is controlled by two independent layers. Both must permit a tool for it to be invocable.

### 1. Per-agent manifest allowlist (deny-by-default)

Every agent ships with a `manifest.json` that lists the exact tools it may use. The runner registers only those tools into that agent's registry. A tool that is not in the manifest is unreachable from the agent — there is no implicit "allow all", and no runner-side flag that turns this off.

This is the layer that gives the runtime its deny-by-default property: the only path to a tool is to add its name to the agent's manifest, rebuild, and republish.

### 2. Cluster-wide access-class cap (optional)

Each host tool declares an access class — `read`, `write`, or `delete` — in its metadata. Operators can cap which classes a runner will expose by setting the `BAML_TOOL_ACCESS_ALLOWLIST` environment variable to a comma-separated list, for example:

```bash
# Permit only read tools cluster-wide; reject write/delete tools at registration.
export BAML_TOOL_ACCESS_ALLOWLIST=read,write
```

When the variable is unset, the cap imposes no extra restriction — the manifest allowlist is still the gate. When set, the cap applies in addition to the manifest: a tool is permitted only if it appears in the agent's manifest **and** its declared access class is in the cap.

The runner logs the resolved cap at boot, e.g.:

```text
Tool access cap resolved (per-agent manifest allowlist still gates tool exposure) env_set=false unrestricted=true permitted=["delete", "read", "write"]
```

Operators can read this line to confirm what cap is active without grepping source.

## Deploy vs Publish

- Publish sends source bundle to `/repository/publish`; server-side build stores artifact bytes under canonical `content_hash`.
- Deploy activates a previously published `content_hash` into the running registry.

Typical flow:

1. Start runner with `--serve-http` and repository flags (see above).
2. `agentium install agent --path agents/clickup-agent --url http://127.0.0.1:18080` (or `agentium publish` then `agentium deploy` with the printed hash).

## End-to-End Example

### Standalone (local development)

Start runner:

```bash
agentium serve \
  --serve-http 127.0.0.1:18080 \
  --repository-url http://127.0.0.1:18080/repository \
  --state-dir ./.runner-state \
  --repository-dir ./.repository \
  --provenance-db provenance.db
```

Publish source:

```bash
agentium install agent \
  --path agents/clickup-agent \
  --url http://127.0.0.1:18080 \
  --runner-token "$RUNNER_TOKEN"
```

Deploy by content hash (the hash printed in publish result):

```bash
curl -sS -X POST http://127.0.0.1:18080/deploy \
  -H 'content-type: application/json' \
  -d '{"hash":"bfe72df219673c1a919817b29c37c2b51419e1e81b61eeca5e5549bd7b1b5d83"}' | jq
```

### Cluster mode (K8s)

In cluster mode, operator actions require `X-Runner-Token`. The token is provisioned as a Kubernetes secret referenced by the Helm chart (see `deploy/helm/agentium-os/`). For local k3d: `just up`.

**Using the CLI (recommended):**

Both `agentium` subcommands accept `--runner-token` (or `RUNNER_TOKEN` env) for authenticated operator access. See [`agentium-cli.md`](agentium-cli.md).

```bash
agentium install agent \
  --path agents/clickup-agent \
  --url http://localhost:18080 \
  --runner-token "$RUNNER_TOKEN"
```

**Using curl:**

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
curl -N -X POST "http://localhost:18080/agents/clickup-agent/default/a2a" \
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
- `POST /agents/{agent_package}/{agent_instance_id}/a2a` (JSON-RPC; SSE streaming on same path)
  Stream A2A responses over SSE.

## SSE Runtime Requirement

SSE stream tasks must run on the same long-lived Tokio runtime as HTTP serving.
Do not spawn short-lived nested runtimes for A2A stream handlers, or clients may
see only keep-alive events.
