# Agentium OS Web UI

A single-page Vue 3 + TypeScript app that serves as the operator and developer console for a running `baml-agent-runner`. It is the visual surface for the host-governed runtime: deploy agents, configure LLM clients / tool bundles / secrets, chat with deployed agents, and inspect the provenance graph generated as agents execute.

The UI talks to a single runner over its HTTP API (A2A JSON-RPC, deployment, configuration, and provenance endpoints) and is shipped alongside the runner — `just web-build` produces the static bundle that the runner can serve.

## What you can do

The app has three top-level views, selectable from the navbar.

### Dashboard

Landing view (`Dashboard.vue` + `dashboard/*` sections). Narrative layout: **Runtime now** (multi-tab lanes, optional planning chip, compact provenance health), **Attention needed** (ranked issues with drill-through to `ProvenancePane` tabs), **Causal story** (focused-lane transcript tail, session metrics, hotspots, trace preview), and **System surface** (runner reachability, agent inventory, configuration entry).

### Chat

Multi-tab chat surface for conversations with deployed agents.

- **Tabs and history** — `ChatTabs.vue` keeps independent A2A clients per tab; `ConversationHistorySelector.vue` lists prior contexts/tasks for an agent and reloads them.
- **Agent picker** — `AgentSelector.vue` lists everything published under `GET /agents`; switching agents prompts before clearing the transcript.
- **Conversation** — `ChatWindow.vue` renders the streaming transcript via `MessageBubble.vue`, surfaces `INPUT_REQUIRED` prompts (`awaitInput`), and shows `WorkflowProgress.vue` while a task is running.
- **Tool / reasoning visibility** — `ToolNotificationCard.vue` and `tool-card/*` render the host's tool-session FSM (Open / Send / Read / Finish / Abort) inline; `ReasoningPane.vue` exposes model reasoning when the provider returns it.
- **Provenance side pane** — `ProvenancePane.vue` mounts next to the chat and tabs through five views over the live context's provenance:
  - **Live** (`ProvenanceLiveTab.vue`) — streaming counts, hotspots, drilldowns
  - **Failures** (`ProvenanceFailuresTab.vue`)
  - **Anomalies** (`ProvenanceAnomaliesTab.vue`)
  - **Drift** (`ProvenanceDriftTab.vue`) — embedding drift signals
  - **Explore** (`ExploreTab.vue`) with `ProvenanceRowInspector.vue` for raw row inspection

### Settings

Operator console (`SettingsView.vue`) with four tabs:

- **LLM** (`ConfigLlmView.vue` + `ConfigLlmForm.vue`) — view and edit the LLM client bundle (providers, models, routing).
- **Tools** (`ConfigToolBundleEditor.vue`) — per-bundle tool configuration backed by the runner's tool config schema.
- **Secrets** (`ConfigSecretsView.vue`) — secrets inventory and linkage to LLM / tool config keys.
- **Deployments** (`DeploymentPanel.vue`) — list deployed agents, deploy by content hash or by repository name+version, and undeploy.

URL state (active view, selected agent, context id) is reflected in the query string so links and browser navigation are stable.

## Running locally

Prereqs: a recent Node LTS (Vite 7 requires Node 20.19+ or 22.12+), npm, and a `baml-agent-runner` reachable on `http://127.0.0.1:18080` (the Vite dev server proxies all runner routes to that origin — see `vite.config.ts`).

```bash
cd web
npm install        # first time
npm run dev        # http://localhost:5173
```

Other scripts (from `package.json`):

| Script             | Purpose                                                          |
| ------------------ | ---------------------------------------------------------------- |
| `npm run dev`      | Vite dev server with HMR; proxies runner routes to `:18080`.     |
| `npm run build`    | Type-checks (`vue-tsc -b`) and emits a static bundle to `dist/`. |
| `npm run preview`  | Serve the built bundle locally.                                  |
| `npm run lint`     | ESLint over `src/`.                                              |
| `npm run lint:fix` | ESLint with autofix.                                             |
| `npm run format`   | Prettier write over `src/**/*.{ts,vue,css}`.                     |
| `npm run test`     | Vitest run (jsdom).                                              |

From the repo root, `just web-build` runs `npm ci && npm run build` so the runner can serve the produced `dist/`.

To start a runner that the UI can talk to, see the project root `CLAUDE.md` and `deploy/helm/agentium-os/README.md`. Locally:

```bash
cargo run -p baml-agent-runner   # serves HTTP on 127.0.0.1:18080 by default
```

## Authentication

The runner splits its HTTP surface into public routes (used by the Chat and Dashboard views — agent discovery, A2A JSON-RPC + SSE, conversation history) and operator routes (used by the Settings view — config, deploy/undeploy, repository mutation, migration). In cluster mode the runner gates operator routes behind an `X-Runner-Token` header. The full route table lives in `docs/agent-runner.md` ("HTTP API Authentication") and the project root `CLAUDE.md`.

The UI does not inject the runner token itself, so the Settings view only works against **local development runners** or **pilot deployments fronted by a proxy that supplies the token**. Do not expose the operator routes — or this UI without an auth proxy in front — to untrusted networks.

## Directory map

```
web/
├── index.html        # SPA shell
├── vite.config.ts    # Dev-server proxy to the runner
├── package.json      # Scripts and dependencies
└── src/
    ├── App.vue       # Top-level layout, view routing, URL state
    ├── main.ts
    ├── components/   # UI components (with provenance/ and tool-card/ subfolders)
    ├── composables/  # A2A client, config/deploy APIs, theme, toasts, mermaid, provenance ops, episode stream
    ├── chat/         # FSM mapping, message/tool block parsing, conversation history hydration
    ├── types/        # TypeScript shapes for A2A, config, provenance
    └── utils/        # Formatting, mermaid parsing, markdown rendering
```
