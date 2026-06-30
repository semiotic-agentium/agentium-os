# Agentium Console (web)

Vue 3 + TypeScript **developer console** for a running **Agentium OS** instance (`baml-agent-runner`). It is a client of the server — not the OS itself. Connect to an instance URL, load agents (publish + deploy), chat, observe host ingress, and inspect provenance.

Full walkthrough: [`docs/runbooks/agentium-console.md`](../docs/runbooks/agentium-console.md).

## Verify (devx + E2E API)

```bash
./scripts/verify-agentium-console.sh
# or: just verify-agentium-console
```

Runs Vitest, production build, runner route checks, and publish+deploy of `task-lifecycle-demo` (same HTTP contract as **Agents → Load agent**).

**Bootstrap + eval:** `./scripts/verify-bootstrap-eval.sh` or `just verify-bootstrap-eval` — scaffolds a fresh agent, publish/deploy, and A2A eval via `bootstrap-echo-eval` (no LLM keys). See [`docs/runbooks/agentium-console.md`](../docs/runbooks/agentium-console.md).

## Two ways to run

| Mode | When | How |
|------|------|-----|
| **Co-served** | Quick demo, single origin | `just web-build && just runner` → open `http://127.0.0.1:18080/` |
| **Standalone Vite** | UI hot reload | Terminal 1: `just runner` · Terminal 2: `cd web && npm run dev` → connect to `http://127.0.0.1:18080` |

The UI uses the runner HTTP API (A2A, repository publish, deploy, config, provenance).

## Connect to an instance

On first load, the console tries the current origin. If that fails (typical Vite dev on `:5173`), enter:

- **Instance URL** — e.g. `http://127.0.0.1:18080`
- **Runner token** — optional locally; required for operator routes in cluster mode (`/config`, `/repository/publish`, `/deploy`)

Credentials persist in `localStorage` (`agentium:instance-url`, `agentium:runner-token`). Click the instance host pill in the navbar to change connection.

For cross-origin standalone access (no Vite proxy), start the runner with:

```bash
AGENTIUM_CORS_ORIGINS=http://localhost:5173 just runner
```

## Views

| View | Purpose |
|------|---------|
| **Agents** | Load agent source folder → `POST /repository/publish` → `POST /deploy`; fleet table |
| **Chat** | Multi-tab A2A conversations with provenance pane |
| **Event Console** | Host ingress publish, subscriber fan-out, observe transcript |
| **Dashboard** | Runtime lanes, attention, causal story (focused tab scope) |
| **Settings** | LLM, tools, secrets, deploy-by-hash |

## Load an agent (browser)

1. Connect to the instance.
2. Open **Agents** → **Select agent folder…** (directory with `manifest.json`, `src/`, `baml_src/`).
3. **Load agent** — same as `baml-agent-builder publish --deploy-url …` (deploy after publish checked by default).

## Frontend architecture

```
instanceApi.ts       → transport only (no Vue)
useInstanceClient    → connection session
useAgentsApi         → discovery + health ping
useConfigApi         → settings + model budgets
useDeployApi         → shared fleet state
usePublishApi        → Agents load flow
Vue components       → composables only (ESLint blocks instanceApi in components/)
```

## Scripts

| Script | Purpose |
|--------|---------|
| `npm run dev` | Vite dev server + API proxy to `VITE_INSTANCE_URL` |
| `npm run build` | `vue-tsc` + production bundle to `dist/` |
| `npm run test` | Vitest |

From repo root: `just web-build`.

Optional: `VITE_INSTANCE_URL=http://127.0.0.1:18080 npm run dev`

## Authentication

Public routes: agent discovery, A2A, conversation history, `/events/publish`. Operator routes need `X-Runner-Token` (entered at connect or injected by a cluster proxy). See [`docs/reference/agent-runner.md`](../docs/reference/agent-runner.md).
