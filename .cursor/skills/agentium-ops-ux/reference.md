# Agentium Ops UX — Reference

## Top-level views (`App.vue`)

| View | URL param | Primary files |
|------|-----------|---------------|
| Dashboard | `?view=dashboard` | `Dashboard.vue`, `dashboard/*` |
| Chat | `?view=chat` (default) | `ChatWindow.vue`, `ChatTabs.vue`, `ProvenancePane.vue` |
| Event Console | `?view=events` | `events/EventConsole.vue`, `useEventConsole.ts` |
| Settings | `?view=settings` | `SettingsView.vue`, `Config*.vue`, `DeploymentPanel.vue` |

Shared URL param: `view`. Scoped agent/context params (do not mix views):

| View | URL params |
|------|------------|
| Chat | `chatAgentPackage`, `chatAgentInstance`, `chatContextId` |
| Event Console | `eventAgentPackage`, `eventAgentInstance`, `eventContextId` |

Legacy `agentPackage` / `agentInstance` / `contextId` are read as fallback for the **active** view only.

**App shell:** `OperatorAgentSelector` below the navbar — label switches between Chat agent and Compose agent. Chat toolbar and Event compose modal do not duplicate agent pickers.

Event Console syncs route via `writeEventConsoleRoute()` → `writeEventConsoleRouteToUrl()` in `events/operatorRoute.ts`.

## ProvenancePane tabs

| Tab | Purpose | Key component |
|-----|---------|---------------|
| Live | Aggregate counts, intent/plan, Mermaid trace, hotspots | `ProvenanceLiveTab.vue` |
| Failures | Failed LLM/tool ops | `ProvenanceFailuresTab.vue` |
| Anomalies | Anomaly signals | `ProvenanceAnomaliesTab.vue` |
| Drift | Embedding drift | `ProvenanceDriftTab.vue` |
| Explore | Raw row inspector | `ExploreTab.vue`, `ProvenanceRowInspector.vue` |

Props: `context-id`, `task-id`, `is-streaming`, `diagrams`, `trace-refresh-tick`, `default-open`, `prefer-open`.

## Event Console layout (observe-first)

```
EventRunHeader     → run picker, status pill, context chip, New event
EventRunStatusBanner → publish outcome / failures / waiting ingress
EventComposeModal  → centered publish dialog (agent, payload, scope, batch)
                   → observe row (event run picker, phase, context/task ids, publish outcome)
events-observe     → TranscriptView (shared with Chat)
ProvenancePane     → default-open right column; sole trace surface
```

**State**: `observedContextId` (picker/publish) is decoupled from `draft.scope` (publish intent). Picker changes do not mutate draft scope; drawer **Target observed run** copies observation into draft.

Transcript is provenance-only (no synthetic publish-summary chat lines).

Key composables:

- `useEventConsole` — draft, validation, publish, history, route sync
- `useEventObservation` — conversation history hydrate, trace refresh
- `dispatchObserve.ts` — scope resolution (`observationSource`: picker | publish | draft), `transcriptHasHostIngress`

Dispatch phases (`dispatchPhases.ts`): `idle` → `publishing` → `recording` → (streaming) → `idle` / `failed`.

## Chat building blocks

| Component | Role |
|-----------|------|
| `MessageBubble.vue` | Agent/user/system messages |
| `ToolNotificationCard.vue` | Host tool session steps |
| `WorkflowProgress.vue` | In-flight task banner |
| `ConversationHistorySelector.vue` | Prior contexts |
| `AgentSelector.vue` | Deployed agent picker |

Ingress messages: `userSpeakerKind === "ingress"` in conversation history API.

## CSS tokens (`web/src/style.css`)

Semantic variables — use these, not raw hex:

- Status: `--color-error`, `--color-warning`, `--color-success`, `--color-accent`
- Citations: `--color-cite-history`, `--color-cite-archive` (+ `-subtle`, `-border` variants)
- Theme: `[data-theme="dark"]` overrides; toggled via `useTheme`

Global: `@media (prefers-reduced-motion: reduce)` already defined — extend, don't fight it.

## Runner API surfaces (UI-relevant)

### Public (Chat, Dashboard, Event Console)

| Endpoint | UI use |
|----------|--------|
| `GET /agents` | Agent discovery |
| `POST /agents/{pkg}/{inst}/chat` | A2A JSON-RPC + SSE |
| `POST /agents/{pkg}/{inst}/dispatch` | Direct dispatch (tests/tools) |
| `GET /contexts` | Context picker lists (`eventOnly=true` on Event Console) |
| `GET /contexts/{id}/conversation-history` | Transcript hydrate |
| `GET /contexts/{id}/conversation-history/stream` | SSE transcript updates |
| `GET /contexts/{id}/mermaid` | Trace diagrams |
| `GET /contexts/{id}/metrics` | Context metrics strip |
| `GET /contexts/{id}/planning` | Dashboard planning chip |
| `GET /message-shapes` | Event Console registry |
| `POST /event-dispatch/validate` | Draft validation |
| `POST /events/publish` | Host event publish |

Query `agent_package` on conversation-history filters transcript to one agent's graph paths.

### Operator (Settings only — token required in cluster)

| Endpoint | UI use |
|----------|--------|
| `GET/POST /config` | LLM/tool configuration |
| `GET /config/secrets-overview` | Secrets inventory |
| `POST /deploy`, `POST /undeploy` | Deployment panel |

## Verification workflow (manual)

1. `just runner` or existing runner on `127.0.0.1:18080`
2. `just web-build` (or `npm run dev` for HMR)
3. Event Console: `/?view=events&agentPackage=clickup-agent&agentInstance=default`
4. Validate → Publish ClickUp source records sample
5. Confirm: publish outcome `1/1 accepted`, HOST INGRESS in transcript, ProvenancePane trace
6. API cross-check: `GET /contexts/{context_id}/conversation-history?agent_package=clickup-agent`

## File map (quick)

```
web/src/
├── App.vue                 # View router + URL state
├── components/
│   ├── Dashboard.vue
│   ├── ProvenancePane.vue
│   ├── events/EventConsole.vue
│   ├── events/EventRunHeader.vue
│   ├── events/EventRunStatusBanner.vue
│   ├── events/EventComposeModal.vue
│   └── dashboard/          # Narrative sections
├── composables/
│   ├── useEventConsole.ts
│   ├── useEventObservation.ts
│   ├── useDashboardViewModel.ts
│   └── useA2aClient.ts
├── events/
│   ├── dispatchObserve.ts
│   ├── dispatchPhases.ts
│   └── messageShapes.ts
└── style.css               # Design tokens
```
