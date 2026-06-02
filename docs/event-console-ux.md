# Event Console — operator UX

The Event Console follows a **CI run page** pattern: a thin control shell, a status strip, a linear provenance transcript (shared with Chat components), and a **centered modal** for publishing.

## Control shell

- **Compose agent** (app shell, below navbar) — Selects the agent used for validate/draft and run-history filtering. Independent from **Chat agent** on the Chat view. Deep links use `eventAgentPackage`, `eventAgentInstance`, and `eventContextId` (legacy `agentPackage` / `contextId` still work on `?view=events`).
- **Run** — Pick a prior `context_id` or publish a new event.
- **Run status** — Shared `OperatorRunStatus` via `RunStatusIndicator` (same model as Chat and ProvenancePane header): phase label, unit progress (`X/Y units`), severity-colored dot. Not separate Live/Idle semantics.
- **New event** — Opens the publish modal (source payload, scope, JSON batch; compose agent shown read-only from the shell).

## After publish

- **Run status strip** — `RunStatusIndicator` banner driven by `deriveEventRunStatus`: preparing → publishing → recording → executing (incomplete units / open tool sessions) → complete. Publish acceptance and subscriber failures stay visible until dismissed; green success only when units are fully processed.
- **Transcript timeline** — One vertical run timeline (`EventTranscriptRow`) with stable row kinds:
  - **Milestone** — Publish acceptance summary (replaces operator chat bubbles for publish trace).
  - **Ingress wire** — Full-width `ingress-wire-card` JSON preview; optimistic local row hydrates in place by stable key (`row:ingress-wire`).
  - **Operational** — Host/system cards (`dispatch_*`, `source_poll_recorded`, failures).
  - **Agent turn** — Agent lane (text, tools, session steps); Chat `MessageBubble` styling is unchanged on the Chat view only.
  - **Skeleton** — Placeholder rows while provenance loads after publish (no empty-state mode flicker).
- **Traces** — Expand the right rail once a run is selected or after publish.

## Transcript vs episode export

- **Primary transcript** (center column) is the operator source of truth: `GET /contexts/{id}/conversation-history` with `profile=full`. It includes host operational rows (`source_poll_recorded`, `dispatch_*`), system failures (`llm_call_failed`, `prompt_rejected`), ingress wire JSON (`user_speaker_kind: ingress`), and agent tool/session rows.
- **Live vs reload parity** — SSE `snapshot` and picker reload both apply the same paginated GET merge through `applyConversationHistoryIngress` (full replace). SSE `delta` does not append incrementally in Event Console; it schedules the same authoritative GET reconcile so live and reload always share one code path. Optimistic ingress rows sit in a local overlay until provenance includes host ingress, then drop automatically.
- **Episode download** (Provenance pane) — plain-text episode export includes operational provenance rows (dispatch failures, poll records, LLM errors). The **`session_history`** / BAML **`conversation_transcript`** projection still omits them so agents do not see operator diagnostics in prompts.

After publish, observation resolves the first `dispatch-unit-*` task id when present so task-scoped transcript and provenance ops align with the dispatch unit episode.

## Validate vs publish

Use **Validate** in the modal to check the draft against `POST /event-dispatch/validate`. **Publish event** calls `POST /events/publish`, which fans out to all matching subscribers (not only the agent selected for validation).
