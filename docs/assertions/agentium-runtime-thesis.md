<!-- doc-type: assertion -->

# Agentium runtime thesis

Normative architectural assertions that guide agent authoring, runtime changes, and reviews.
Validated against the codebase as of 2026-06.

## Runtime boundary

**Agentium OS** is a deployable **agent runtime**, not an in-process framework. It executes
declarative agent artefacts (manifest, TypeScript, BAML), owns planning and execution surfaces,
orchestrates **host-mediated** tool sessions, and captures provenance for everything inside the
boundary.

External systems stay outside the boundary. What the runtime records is what crosses host-visible
interfaces: LLM calls, tool hops, A2A interactions, task transitions, messages, artefacts, and
provenance events.

## Host-mediated effects

Host tools run in Rust via the session FSM (`Open` / `Send` / `SearchRead` / `PageRead` / `Finish` / `Abort`).
JavaScript never mediates host tool execution. Because effects are host-mediated, observability
within the boundary is complete: every agent action passes through runtime machinery.

## Agent artefacts

The canonical shareable unit is a **content-addressable source bundle** (manifest, TypeScript,
BAML). It is declarative content executed by the trusted runtime without an external dependency
graph of its own.

## Planning and structured outcomes

Agents are ReAct-capable task actors with enforced planning semantics: tasks, plans, steps,
tool-session phases, citations, lineage, and `StructuredReply` are **runtime concepts**, not
informal prompt conventions. Coordinator product plans must not reuse tool-session FSM shapes at
the top level.

## Graph-first provenance reads

All provenance read paths (conversation context, episode assembly, drift scoring, graph export)
must reconstruct data by **traversing graph edges** — never by parsing node ID prefixes,
matching timestamps, or building HashMap joins from string keys. If a read path needs a
relationship that is not expressed as an edge, fix the write path.

## Conversation boundaries

Three boundaries must stay separate (see [`baml-rt-conversation-spec.md`](baml-rt-conversation-spec.md)):

| Boundary | Owns | Must not |
|----------|------|----------|
| **CanonicalHistory** | Graph-derived `Message` / `ToolCall` / `SessionStep` rows | Mix FSM prose or invent/drop activities |
| **PhaseOverlay** | Transient phase-specific prompt material | Appear in `conversation_transcript` |
| **StableHistoryRefResolver** | `#N` / `@N` ref table, idempotent on reprojection | Advance `#N` on full-graph re-read |

## Cluster continuity

For clustered deployments, **router-held stateful conversations** hold client continuity at the
edge. Cross-pod A2A forwarding, shared SurrealDB provenance, and deployment lifecycle APIs are
the operational substrate. Durable truth is the provenance graph; in-process SSE broadcast is
best-effort with graph backfill on reconnect.

## Further reading

| Topic | Doc |
|-------|-----|
| Agent authoring | [`how-to-write-agents.md`](how-to-write-agents.md) |
| Conversation invariants | [`baml-rt-conversation-spec.md`](baml-rt-conversation-spec.md) |
| Event delivery | [`host-to-agent-event-delivery.md`](host-to-agent-event-delivery.md) |
| Rust conventions | [`production-rust.md`](production-rust.md) |
| Architecture overview (as-is) | [`README.md`](../../README.md) |
