# Host-to-Agent Event Delivery

## Overview

Agentium OS separates **event intake** from **event meaning**. The host owns intake, provenance, subscription matching, and delivery. Agents own meaning and downstream decisions.

Every tool is a potential event source. A tool declares what event kinds it can produce via `event_sources` metadata. Agent subscriptions filter by schema version and source kind. The host multiplexes event-producing tools and dispatches to matching agents.

This is the Unix model: a tool is a bidirectional interface (invoke and optionally produce events), the host is `epoll`.

## The Model

Tools have two roles:

1. **Invoke** (agent -> tool): the agent calls the tool through the session FSM (Open/Send/Next/Finish/Abort).
2. **Produce events** (tool -> host -> agent): the tool declares `event_sources` in its metadata, the host polls or receives webhooks, and dispatches matching events to subscribed agents.

```
┌─────────┐   invoke    ┌──────┐   poll/webhook   ┌──────┐
│  Agent   │ ──────────> │ Tool │ <──────────────── │Source│
└─────────┘             └──────┘                   └──────┘
     ^                      │
     │    dispatch           │ events
     └───── host ◄───────────┘
```

A tool's `ToolFunctionMetadata.event_sources` field declares which `EventSourceKind` values it can produce. Empty means invoke-only. When a tool declares event sources, the host can poll it and route resulting events to agents whose subscriptions match.

## Subscription Declaration

Agents declare subscriptions in `manifest.json` under `discovery.subscriptions`:

```json
{
  "discovery": {
    "subscriptions": [
      {
        "schema_versions": ["host.source-records.v1"],
        "source_kinds": ["slack", "clickup"]
      }
    ]
  }
}
```

### Matching Rules

- **OR within a field**: any listed schema version or source kind matching is sufficient.
- **AND across fields**: when both `schema_versions` and `source_kinds` are present, a subscription must satisfy both.
- `schema_versions` are case-sensitive.
- `source_kinds` are matched case-insensitively (lowercased on parse).
- `source_keys` and `source_key_prefixes` provide optional narrower matching.
- An entirely empty subscription entry is ignored for event delivery.

See `crates/baml-rt-core/src/event_subscription.rs` for the `EventSubscription` type and matching logic.

## Dispatch Protocol

The host delivers events to agents via `AgentDispatchRequest`:

```rust
pub struct AgentDispatchRequest {
    pub routing_key: AgentDispatchRoutingKey,  // e.g. "slack:intake"
    pub message_type: EventSchemaVersion,       // e.g. "host.source-records.v1"
    pub messages: Vec<Value>,                   // opaque event payloads
    pub context_id: Option<ContextId>,          // continue existing context
    pub task_id: Option<TaskId>,                // continue existing task
    pub message_id: Option<String>,             // provenance continuity
    pub metadata: Option<DispatchMetadata>,     // structured transport metadata
}
```

The agent responds with `AgentDispatchAck { accepted: bool, detail: Option<String> }`.

**HTTP endpoint**: `POST /agents/{pkg}/{inst}/dispatch` (single-agent delivery; programmatic and callback paths).

**Publish ingress**: `POST /events/publish` accepts a `ProducedEvent`, records `HostSourcePollRecorded` in provenance, matches all deployed agent subscriptions, and fans out dispatch to each subscriber (recording `HostDispatchAccepted` per accept). This is the same path **task-daemon** and the **Event Console** use.

See `crates/baml-rt-core/src/dispatch.rs` for types and `crates/baml-rt-api/src/handlers.rs` for the HTTP handlers.

## Event Console (operator simulate)

The web **Event Console** validates drafts via `POST /event-dispatch/validate` (returns `preview_produced_event`) and publishes via `POST /events/publish` — not direct per-agent dispatch. Operator evidence is the **conversation-history transcript** at the pinned `context_id`: graph-backed `Message` rows with role `host` for poll and dispatch-accept lines (`Host source poll: …`, `Host dispatch accepted: …`), then agent A2A rows as they arrive.

## Agent Handler

Agents implement dispatch handling via `__chat_register`:

```typescript
__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    // A2A conversational entrypoint
  },
  onDispatch: async (request: HostDispatchRequest): Promise<HostDispatchAck> => {
    // Event delivery entrypoint
    return { accepted: true, detail: "processed" };
  },
});
```

The runtime wires `onDispatch` onto `globalThis`. When a dispatch request arrives, the runtime calls the agent's `onDispatch` handler directly, bypassing the A2A conversational path.

### Reference Fixtures and Product Agents

- **`dispatch-echo`** (`tests/fixtures/agents/dispatch-echo/`): Minimal dispatch handler that echoes routing key and message count.
- **`slack-agent`** (`agents/slack-agent/`): Subscribes to `host.source-records.v1` / `slack`; semantic ingress may delegate PM work via `system/internal_a2a`.
- **`clickup-agent`** (`agents/clickup-agent/`): Subscribes to `host.source-records.v1` / `clickup`; `onDispatch` parses `ClickupSourceRecordsBatch` lifecycle rows and reuses the same BAML rail as chat (`InferClickUpIntent` → `PlanClickUpWork` → `ChooseClickUpAction`).
- **`task-lifecycle-demo`** (`tests/fixtures/agents/task-lifecycle-demo/`): Full conversational lifecycle with both `run` and `onDispatch`.

## Provenance

`HostSourcePollRecorded` and `HostDispatchAccepted` are **event-level** lineage only (ops/Mermaid), not transcript rows. For `host.source-records.v1`, actionable ingress text is **only** written per dispatch unit in each `withTask` prelude (task-scoped `ingress-unit-user` `Message`); publish does **not** also emit a global poll-batch `user` line. Each `withTask` prelude injects the unit’s `records[]` slice as **wire JSON** (delimiter `--- host.source-records.v1 ---` + pretty-printed `{ "records": [...] }`) — the same fields the agent receives in dispatch `messages[0]`; the host does **not** rewrite records into title/description summaries. Agents own interpretation in `onDispatch` / BAML. Dispatch requests carry optional `context_id`, `task_id`, and `message_id`; scope is built via `invocation_scope_for_agent_dispatch` in `crates/baml-rt-core/src/dispatch.rs`.

**Record kinds (wire):** `slack.message`, `clickup.lifecycle_event`, `github.issue_event` — see `crates/tools/*/src/source_records.rs`.

For **`system/callback`** detached continuation, the host mints a **child** dispatch `context_id` / `task_id` on the emitted event while storing the **scheduling** A2A scope separately. Callback delivery deferral uses **only** the scheduling scope (with `requesting_agent_id`), not the minted dispatch ids. When dispatch is accepted, the runner may record a provenance link from the dispatch task to the scheduling task (`WAS_SCHEDULED_FROM`); see `crates/baml-rt-provenance/PROV_MAPPING.md`.

## Discovery

Two system tools surface event-related metadata:

- **`system/discover_tools`**: Returns `ToolDiscoveryRecord` which includes `event_sources` — the event source kinds each tool can produce.
- **`system/discover_agents`**: Returns agent cards with `subscriptions`, filterable by `requiredSchemaVersions` and `requiredSourceKinds`.

## Current State

The **task-daemon** polls external sources (Slack, ClickUp, GitHub) and publishes `host.source-records.v1` via `POST /events/publish`. The runner records provenance and dispatches to agents subscribed on `event:intake`. **`clickup-agent`** is the production subscriber for `source_kinds: ["clickup"]` and processes lifecycle batches in `onDispatch` without requiring a separate ingress-only package.

Production tools can now declare `event_sources` in their metadata and register host-managed producers through inventory. `support/slack` is the first production tool wired through the generic producer path, while `internal-dev/get_weather` remains the minimal metadata/discovery example.

## Slack As A Source Of Work

Slack is a good example of the boundary this system is trying to enforce:

1. The host-managed `support/slack` producer polls configured channels and emits raw `host.source-records.v1` (plus one poll `user` history line for the batch).
2. `slack-agent` `onDispatch` groups raw records into conversation units and calls `withTask({ unitKey, records })` per unit — **no host enrichment** of those records.
3. Inside each unit handler, the agent runs BAML (`InferSlackIntent`, planning) against `conversation_transcript` and may open `support/slack` tool sessions when the model needs thread replies or history.
4. Downstream PM delegation happens only after agent BAML/tool steps, not from host-side pre-interpretation.

The raw poll batch is often not the work unit; the work unit is one conversation slice per `withTask`, with meaning produced by the agent’s LLM and tools.

The coordinator is still downstream of this flow. It is not the universal raw-event sink.

## Adding a New Event Source

1. **Declare `event_sources`** on the tool:
   ```rust
   #[baml_tool(
       name = "support/slack",
       description = "Read Slack messages and channels.",
       tags = ["support", "slack", "read"],
       event_sources = ["slack"],
   )]
   ```
2. **Implement polling or webhook logic** in the tool or a companion daemon.
3. **Produce `AgentDispatchRequest`** with the appropriate `routing_key`, `message_type`, and `messages`.
4. The dispatch path is already source-agnostic — the host matches subscriptions and delivers to agents regardless of which tool produced the event.
