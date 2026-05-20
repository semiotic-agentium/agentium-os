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
        "schema_versions": ["task-daemon.interpretation.v1"],
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
    pub message_type: EventSchemaVersion,       // e.g. "task-daemon.interpretation.v1"
    pub messages: Vec<Value>,                   // opaque event payloads
    pub context_id: Option<ContextId>,          // continue existing context
    pub task_id: Option<TaskId>,                // continue existing task
    pub message_id: Option<String>,             // provenance continuity
    pub metadata: Option<DispatchMetadata>,     // structured transport metadata
}
```

The agent responds with `AgentDispatchAck { accepted: bool, detail: Option<String> }`.

**HTTP endpoint**: `POST /agents/{pkg}/{inst}/dispatch`

See `crates/baml-rt-core/src/dispatch.rs` for types and `crates/baml-rt-api/src/handlers.rs` for the HTTP handler.

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

### Reference Fixtures

- **`dispatch-echo`** (`tests/fixtures/agents/dispatch-echo/`): Minimal dispatch handler that echoes routing key and message count.
- **`task-lifecycle-demo`** (`tests/fixtures/agents/task-lifecycle-demo/`): Full conversational lifecycle with both `run` and `onDispatch`.

## Provenance

Dispatch requests carry optional `context_id`, `task_id`, and `message_id` for provenance threading. The A2A transport layer creates scope from these fields via `scope_from_dispatch_request` in `crates/baml-rt-a2a/src/a2a_transport.rs`.

For **`system/callback`** detached continuation, the host mints a **child** dispatch `context_id` / `task_id` on the emitted event while storing the **scheduling** A2A scope separately. Callback delivery deferral uses **only** the scheduling scope (with `requesting_agent_id`), not the minted dispatch ids. When dispatch is accepted, the runner may record a provenance link from the dispatch task to the scheduling task (`WAS_SCHEDULED_FROM`); see `crates/baml-rt-provenance/PROV_MAPPING.md`.

## Discovery

Two system tools surface event-related metadata:

- **`system/discover_tools`**: Returns `ToolDiscoveryRecord` which includes `event_sources` — the event source kinds each tool can produce.
- **`system/discover_agents`**: Returns agent cards with `subscriptions`, filterable by `requiredSchemaVersions` and `requiredSourceKinds`.

## Current State

The **task-daemon** is the first event producer. It polls external sources (Slack, ClickUp, GitHub) and produces `task-daemon.interpretation.v1` events, dispatching them to subscribed agents.

Production tools can now declare `event_sources` in their metadata and register host-managed producers through inventory. `support/slack` is the first production tool wired through the generic producer path, while `internal-dev/get_weather` remains the minimal metadata/discovery example.

## Slack As A Source Of Work

Slack is a good example of the boundary this system is trying to enforce:

1. The host-managed `support/slack` producer polls configured channels and emits raw `host.source-records.v1`.
2. Semantic ingress receives those raw records and groups them into conversation units, typically by `thread_ts` or root message timestamp.
3. When a conversation looks work-like but the raw batch is incomplete, `slack-agent` source ingress can call `support/slack` as a normal host tool to fetch thread replies before deriving work.
4. Only after that interpretation step should downstream work be created or delegated.

That means the raw poll batch is not itself the work unit. The work unit is the interpreted conversation.

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
