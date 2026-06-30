# Agentium Console (web)

Vue 3 + TypeScript **developer console** for a running **Agentium OS** instance. It is a client of the server — not the OS itself. Connect to an instance URL, load agents (publish + deploy), chat, observe host ingress, and inspect provenance.

The UI uses the runner HTTP API (A2A, repository publish, deploy, config, provenance). It can be:

- **Co-served** — `just web-build` and `agentium serve` serves `web/dist` at `/` (same-origin; auto-connect on load).
- **Standalone** — `npm run dev` on `:5173` with Vite proxy to `VITE_INSTANCE_URL` (default `http://127.0.0.1:18080`).

## Connect to an instance

On first load, the console tries the current origin. If that fails (typical Vite dev), enter:

- **Instance URL** — e.g. `http://127.0.0.1:18080`
- **Runner token** — optional; required for operator routes (`/config`, `/repository/publish`, `/deploy`)

Credentials persist in `localStorage`. Click the instance host pill in the navbar to change connection.

For cross-origin standalone access, start the runner with:

```bash
AGENTIUM_CORS_ORIGINS=http://localhost:5173 agentium serve --serve-http 127.0.0.1:18080
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
3. **Load agent** — same as `agentium install agent` (publish + deploy when checked).

## Running locally

```bash
# Terminal 1 — Agentium OS
cargo run -p agentium -- serve --serve-http 127.0.0.1:18080

# Terminal 2 — console (proxied)
cd web && npm install && npm run dev
```

Optional: `VITE_INSTANCE_URL=http://127.0.0.1:18080 npm run dev`

## Scripts

| Script | Purpose |
|--------|---------|
| `npm run dev` | Vite dev server + API proxy |
| `npm run build` | `vue-tsc` + production bundle to `dist/` |
| `npm run test` | Vitest |

From repo root: `just web-build`.

## Authentication

Public routes: agent discovery, A2A, conversation history, `/events/publish`. Operator routes need `X-Runner-Token` (entered at connect or injected by a cluster proxy). See `docs/reference/agent-runner.md`.
