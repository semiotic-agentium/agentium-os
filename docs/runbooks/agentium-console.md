# Agentium Console (local dev + E2E)

The **Agentium Console** (`web/`) is a Vue client for a running **Agentium OS** instance (`baml-agent-runner`). It is not co-extensive with the runtime — connect to an instance URL, load agents, then chat and observe.

See also [`web/README.md`](../../web/README.md) and [`agent-runner.md`](../reference/agent-runner.md).

## Prerequisites

- Node.js 20+ and `npm` (for `web/`)
- Rust toolchain (`just build` or `cargo build -p baml-agent-runner`)
- Optional: `jq` for manual curl inspection

## Quick verify (CI-style)

From repo root:

```bash
./scripts/verify-agentium-console.sh
```

Or skip web rebuild when iterating on runner-only changes:

```bash
./scripts/verify-agentium-console.sh --skip-web
```

This runs Vitest + `vue-tsc` build, starts a runner if needed, checks console API routes, and publish+deploys `task-lifecycle-demo` (same HTTP contract as **Agents → Load agent**).

Equivalent: `just verify-agentium-console`

## Bootstrap a new agent + eval

Create a package with the builder, load it on the server, and run smoke checks:

```bash
cargo run -p baml-rt-builder --bin baml-agent-builder -- bootstrap ./my-agent \
  --name "My Agent" --description "What it does" --no-tools

# Exploratory CI smoke (temp agent only — nothing committed to the repo):
./scripts/verify-bootstrap-eval.sh
# or: just verify-bootstrap-eval
```

The verify script bootstraps under `/tmp`, patches `src/index.ts` to a deterministic echo (`eval:pass:…`), publish+deploys, and sends one A2A message. For LLM-backed agents, customize `src/index.ts` / BAML after bootstrap instead.

## Dev mode A — co-served (simplest)

Single origin; console auto-connects to the runner serving it.

```bash
just web-build          # web/dist
just runner             # http://127.0.0.1:18080/ serves dist when present
```

Open `http://127.0.0.1:18080/` — no connection form unless auto-connect fails.

## Dev mode B — standalone Vite (hot reload)

Two terminals; Vite proxies API paths to the runner (see `web/vite.config.ts` `PROXY_PREFIXES`).

**Terminal 1 — runner**

```bash
just runner
# or: just runner-provenance  # file-backed provenance for Event Console / graphs
```

**Terminal 2 — console**

```bash
cd web && npm install && npm run dev
```

Open `http://localhost:5173`. On first load, connect with:

- **Instance URL:** `http://127.0.0.1:18080`
- **Runner token:** leave blank for local dev (operator routes open without auth locally)

Vite proxies `/agents`, `/repository`, `/deploy`, `/events`, `/contexts`, `/healthz`, etc. to `VITE_INSTANCE_URL` (default `http://127.0.0.1:18080`).

## Dev mode C — cross-origin (no Vite proxy)

When the console is hosted on a different origin than the runner, start the runner with CORS:

```bash
AGENTIUM_CORS_ORIGINS=http://localhost:5173 \
  just runner
```

Enter the full instance URL in the connection shell. Production alternative: reverse-proxy both under one origin.

## Load an agent (browser)

1. **Connect** to the instance (skipped when co-served on `:18080`).
2. Open **Agents**.
3. **Select agent folder…** — pick a directory with `manifest.json`, `src/**/*.ts`, `baml_src/**/*.baml` (same layout as CLI `--agent-dir`).
4. **Load agent** — `POST /repository/publish` then `POST /deploy` when **Deploy after publish** is checked.

CLI equivalent:

```bash
cargo build -p baml-rt-builder --bin baml-agent-builder
./target/debug/baml-agent-builder publish \
  --agent-dir tests/fixtures/agents/task-lifecycle-demo \
  --repository-url http://127.0.0.1:18080/repository \
  --deploy-url http://127.0.0.1:18080
```

## Operator workflow after load

| Step | View | Action |
|------|------|--------|
| 1 | **Agents** | Confirm fleet row; **Chat** link |
| 2 | **Chat** | Select agent; send `message.sendStream` |
| 3 | **Event Console** | Publish host ingress; observe transcript |
| 4 | **Settings** | LLM / tools / secrets (needs token in cluster mode) |

## Architecture (frontend layers)

```
instanceApi.ts          transport (URL, token, fetch)
  ↓
useInstanceClient       connect / disconnect / gating
useAgentsApi            GET /agents, ping
useConfigApi            /config, model budgets
useDeployApi            fleet list, deploy / undeploy
usePublishApi           publish → deploy (Agents view)
  ↓
Vue components          no direct instanceApi imports (ESLint enforced)
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Connection shell loops | Runner not up; check `curl http://127.0.0.1:18080/agents` |
| Settings / Load agent 401/403 | Enter `X-Runner-Token` at connect (cluster) or use local runner without auth |
| Event Console empty in Vite | Ensure proxy includes `/events`, `/event-dispatch` (updated in `vite.config.ts`) |
| CORS error on `:5173` | Set `AGENTIUM_CORS_ORIGINS` on runner or use Vite proxy mode B |
| Publish timeout | Server build can take minutes; console uses 10 min timeout |
| Fleet table stale after load | Fixed: shared `useDeployApi` state refreshes on deploy |

## Related

- [`verify-runner-http.sh`](../../scripts/verify-runner-http.sh) — broader runner HTTP smoke (A2A stream)
- [`verify-bootstrap-eval.sh`](../../scripts/verify-bootstrap-eval.sh) — bootstrap + publish + deploy + A2A eval
- [`repository-api-contract.md`](../reference/repository-api-contract.md) — publish payload shape
- [`sdk-cli.md`](../reference/sdk-cli.md) — `baml-agent-builder publish`
