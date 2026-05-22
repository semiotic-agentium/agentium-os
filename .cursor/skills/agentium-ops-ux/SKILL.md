---
name: agentium-ops-ux
description: UX and visual design guidance for Agentium OS operator surfaces (Dashboard, Chat, Event Console, Settings, ProvenancePane). Use when designing, building, reviewing, or refactoring web/ UI for data-heavy runtime observability, host ingress, provenance drilldown, or operator workflows.
---

# Agentium OS — Operational UX

Design intelligence for the **Agentium operator console** (`web/`): a Vue 3 SPA over `baml-agent-runner` that surfaces agents, host ingress, provenance graphs, and cluster configuration. Optimized for **dense operational data**, **causal drill-down**, and **trust in persisted graph state** — not marketing pages.

Inspired by general UI/UX practice ([ui-ux-pro-max](https://github.com/nextlevelbuilder/ui-ux-pro-max-skill)); **this skill overrides generic SaaS/landing-page advice** when they conflict with Agentium patterns below.

## When to Apply

**Must use** when changing how something **looks, feels, moves, or is interacted with** in `web/`:

- Dashboard, Chat, Event Console, Settings, ProvenancePane, tool cards, message bubbles
- New operator workflows (publish, validate, deploy, observe)
- Information hierarchy for metrics, transcripts, traces, or errors
- Accessibility, loading/streaming states, or URL deep-linking in the console

**Skip** for pure backend/Rust/API work with no UI impact.

## Product Model (read first)

Agentium is an **operator + developer console**, not a consumer chat app.

| Mental model | UI implication |
|--------------|----------------|
| Provenance graph is authoritative | Traces/diagrams say "from persisted graph"; never fake execution flows |
| Host governs tools | Tool-session FSM (Open/Send/Read/Finish/Abort) is first-class UI |
| Public vs operator routes | Chat/Dashboard/Event Console use public APIs; Settings uses token-gated operator APIs |
| Context = conversation scope | `context_id` threads chat, history, provenance, Event Console observation |
| Ingress ≠ chat user | Host ingress lines use distinct labeling (`userSpeakerKind: ingress`, HOST INGRESS) |

Full view/component map: [reference.md](reference.md).

## Priority Rules (Agentium-specific)

Apply in order when reviewing or designing:

| P | Category | Agentium checks |
|---|----------|-----------------|
| 1 | **Truthfulness** | UI reflects API/provenance state; loading/error/empty are explicit; no stale scope after agent switch |
| 2 | **Operational clarity** | IDs copyable; phase labels (Validate → Publish → Recording); subscriber accept/fail counts visible |
| 3 | **Drill-down path** | Summary → detail without dead ends; Dashboard attention items open ProvenancePane tab or Chat |
| 4 | **Density without noise** | Compact metrics strips; demote duplicate hero cards; progressive disclosure (Form/JSON, collapse panes) |
| 5 | **Streaming & latency** | SSE/stream states; disable destructive actions while in-flight; skeleton/shimmer for >1s graph fetches |
| 6 | **Accessibility** | Focus rings, skip link, aria on tabs/regions, contrast on semantic tokens, `prefers-reduced-motion` |
| 7 | **Deep linking** | URL encodes `view`, `agentPackage`, `agentInstance`, `contextId` where applicable |
| 8 | **Visual system** | CSS variables in `style.css`; dark mode via `useTheme`; semantic colors (error/warning/success/ingress cite) |

## View Patterns

### Dashboard (narrative IA)

Four sections — do not reintroduce parallel hero metric grids:

1. **Runtime now** — lane snapshots, optional planning chip, compact provenance health
2. **Attention needed** — ranked issues with drill-through
3. **Causal story** — transcript tail, session metrics, hotspots, trace preview
4. **System surface** — runner online, agent inventory, settings entry

See `useDashboardViewModel.ts` header comment for legacy → narrative mapping.

### Chat + ProvenancePane

- **Left**: streaming transcript, tool cards, workflow progress, `INPUT_REQUIRED` prompts
- **Right** (optional): ProvenancePane tabs — Live, Failures, Anomalies, Drift, Explore
- Toggle traces persists in `localStorage` (`agentium:showTraces`)
- History selector reloads prior contexts; switching agent confirms before clearing

### Event Console (observe-first, CI run shell)

CI-style control shell + linear transcript + central compose modal:

1. **Run header** (`EventRunHeader`) — Run picker, status pill, context chip (copy), **New event** CTA; hint when recent runs exist but none selected
2. **Status banner** (`EventRunStatusBanner`) — Fleet publish outcome, failures list, waiting-for-ingress, in-flight progress
3. **Transcript** (primary, `flex: 2`) — `TranscriptView` + `MessageBubble` (same stack as Chat); ingress wire JSON in full-width card
4. **Traces** — `ProvenancePane` `surface="event"`; collapsed until `context_id`; `prefer-open` after publish or run pick
5. **Compose modal** (`EventComposeModal`, Teleport) — Agent, source payload, scope segments, batch editor, Validate + Publish event

**Flow**: New event → modal → Validate → Publish → banner + transcript hydrate from `GET /contexts/{id}/conversation-history`. Sticky observed run until user picks another or publishes.

**Ingress display**: Wire JSON in `ingress-wire-card`; host summary uses neutral ingress styling, not chat-blue user bubbles.

**Publish API**: `POST /events/publish` fans out to subscribers; draft agent is for validate targeting — banner says “N of M subscriber(s) accepted”.

### Settings (operator)

Tabbed operator surface: LLM, Tools, Secrets, Deployments. Assume token proxy in cluster; surface auth failures clearly.

## Component Conventions

- **Panels**: `.panel`, border `var(--border)`, `min-height: 0` in flex layouts for scroll containment
- **Phases**: Use shared dispatch phase helpers (`dispatchPhases.ts`); show human labels ("Publishing to subscribers…")
- **IDs**: Truncate middle for display; full id on copy; monospace for `context_id`, task ids, content hashes
- **Mermaid traces**: Generated on demand from graph; show disclaimer text; Download when task complete
- **Toasts**: `useToast` for non-blocking confirm/errors; inline errors for form validation
- **Errors**: RFC 7807-style text from API shown verbatim near the action that failed

## Data Display

| Data type | Pattern |
|-----------|---------|
| Timestamps | Relative in lists; absolute on hover/detail where helpful |
| Token/context metrics | Compact (`formatCompact`); session vs current distinguished |
| Tool sessions | Phase-colored cards; op kind visible (open/send/read/finish/abort) |
| Hotspots / failures | Ranked list with severity; click opens Explore or Failures tab |
| Message shapes | Registry-driven (`GET /message-shapes`); samples prefill draft |
| Long JSON | Form/JSON toggle; validate before publish; preview produced event collapsible |

## Review Checklist

Before shipping UI changes:

```
Truth & state
- [ ] Empty, loading, error, and success states all handled
- [ ] Agent/context scope correct after navigation or publish
- [ ] Streaming ends cleanly (no stuck "Publishing…")

Operational UX
- [ ] Operator can copy context_id / task_id
- [ ] Publish outcome shows matched/accepted/failures
- [ ] Ingress transcript distinguishable from conversational user lines

Layout
- [ ] Flex children scroll internally (no double page scroll)
- [ ] Provenance pane collapsible; works at 1280px and 1920px widths

A11y
- [ ] Keyboard reachable controls; visible focus
- [ ] Regions/headings for trace and transcript areas
- [ ] Color + text/icon for status (not color alone)

Deep links
- [ ] Refresh/back restore view + agent + context where designed

Tests
- [ ] Vitest for pure helpers (message shapes, dispatch observe, phases)
- [ ] Screenshot in PR description for visible web/ changes (per AGENTS.md)
```

## Implementation Notes

- Stack: Vue 3 + TypeScript, Vite, Vitest, Prettier/ESLint
- Prefer **composables** for API/state (`useEventConsole`, `useEventObservation`, `useA2aClient`, `useProvenanceOps`)
- Reuse **ProvenancePane** across Chat and Event Console — do not fork trace UI
- Build: `just web-build`; runner serves `web/dist` at `/`
- Local dev: `npm run dev` proxies to `127.0.0.1:18080`

## Anti-Patterns (Agentium)

- ❌ Decorative animation on trace/graph panels
- ❌ Mock execution diagrams not tied to provenance API
- ❌ Hiding failed subscriber deliveries behind toast-only feedback
- ❌ Duplicating session metrics in Dashboard heroes and causal section
- ❌ Raw hex colors in components (use CSS variables)
- ❌ Placeholder-only form labels in Event Console schema forms
- ❌ Blocking entire console during single context history fetch (use abort/timeouts)
- ❌ Generic "User" styling for host ingress records

## Additional Resources

- [reference.md](reference.md) — views, components, URL params, API endpoints
- [web/README.md](../../web/README.md) — local runbook
- [useDashboardViewModel.ts](../../web/src/composables/useDashboardViewModel.ts) — dashboard IA rationale
