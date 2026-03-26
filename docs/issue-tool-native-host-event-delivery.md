# Issue: Make Host-to-Agent Event Delivery Tool-Native and Source-Agnostic

## Summary

`docs/host-to-agent-event-delivery.md` is the correct architecture and should be treated as the target model for the platform.

Today, the codebase has the right pieces in isolation:

- tools can declare `event_sources`
- agents can declare event subscriptions
- the host can deliver `AgentDispatchRequest` to an agent's `onDispatch`
- task-daemon can publish interpreted events and route them to subscribed agents

What is still missing is the platform-level bridge that makes those pieces work together in the general case.

As a result, users who add a new event-producing tool cannot simply declare `event_sources`, implement polling or webhook intake, and rely on the host to deliver events to subscribed agents. They still need bespoke task-daemon-style plumbing or custom source-specific runtime code.

This issue is to close that gap and make reality match the documented model.

## Why This Matters To Users

From a user perspective, the platform promise is:

- I can expose a source of external events as a tool.
- I can declare what source kinds that tool produces.
- I can build agents that subscribe to those source kinds and schema versions.
- The host will take care of intake, provenance, matching, and delivery.

That promise is not fully true today.

The current user experience is fragmented:

- tool metadata says a tool can produce events
- discovery can show those event source kinds
- agent subscriptions can describe what the agent wants
- but there is no source-agnostic host runtime path that turns those declarations into actual delivery

This makes the platform harder to extend, because each new source risks becoming another special-case daemon or custom dispatch path instead of participating in one coherent runtime model.

## Problem Statement

We need a first-class host runtime model for event-producing tools so that:

- tool-declared `event_sources` are not just discoverable metadata, but operational capability
- event intake is owned by the host rather than duplicated in bespoke source-specific code
- agent subscription matching remains declarative and source-agnostic
- dispatch delivery remains generic and uses the existing `AgentDispatchRequest` and `onDispatch` contract
- provenance and delivery semantics remain strong enough that events are not silently dropped or detached from their originating context

The implementation should converge the platform on the documented mental model:

- tools are bidirectional interfaces
- the host is the intake and delivery orchestrator
- agents own meaning and downstream decisions

## What Exists Today

Relevant implementation pieces already exist:

- `EventSourceKind`, `EventSubscription`, and matching logic live in `crates/baml-rt-core/src/event_subscription.rs`
- tool metadata now includes `event_sources` in `crates/baml-rt-tools/src/tools.rs`
- the `#[baml_tool]` macro supports `event_sources` in `crates/baml-tool-derive`
- `system/discover_tools` returns `event_sources`
- `system/discover_agents` can filter by required schema versions and source kinds
- the agent runtime wires `onDispatch` onto `globalThis` and invokes it through `AgentDispatchRequest`
- task-daemon already performs a source-specific version of source polling, subscription matching, and dispatch delivery

This means the issue is not to invent the model from scratch.

The issue is to connect these existing capabilities into one platform abstraction that makes event delivery tool-native rather than task-daemon-specific.

## The Core Gap

PR `#118` made the metadata and discovery layer real.

What is still not true in the general runtime is:

- declaring `event_sources` on a tool does not make that tool operational as a host-managed event producer
- there is no explicit host abstraction for poll/webhook producers that integrates with tool registration
- there is no general event-producer registry or scheduler derived from host tool metadata
- there is no generic bridge from "tool can produce source kind X" to "host will intake events from that tool and dispatch them to subscribed agents"

The only end-to-end producer path today is still the special-case task-daemon flow.

## Desired Outcome

A user adding a new source should be able to do something like this:

1. Register a host tool such as `support/slack` or `support/notion`.
2. Declare `event_sources = ["slack"]` or similar in tool metadata.
3. Implement an explicit polling or webhook intake path for that producer.
4. Have the host discover that producer, ingest events, attach provenance, match subscribers, and deliver dispatches to agents with matching subscriptions.

The user should not need to build a parallel one-off event delivery system just to make the source usable.

## Architectural Invariants

Any solution must preserve these invariants.

### Platform Ownership

- The host owns event intake, subscription matching, provenance threading, and delivery.
- Agents own semantic interpretation and downstream action selection.
- The event path must remain non-conversational and use `onDispatch`, not the conversational A2A message path.

### Contracts That Must Remain Stable

- `AgentDispatchRequest` and `AgentDispatchAck` remain the delivery contract.
- `POST /agents/{agent_package}/{agent_instance_id}/dispatch` remains the HTTP delivery boundary.
- `__chat_register({ onDispatch })` remains the agent entrypoint for host-delivered events.
- `EventSubscription` matching semantics must not regress:
  - OR within each field
  - AND across fields
  - `schema_versions` are case-sensitive
  - `source_kinds` are normalized/lowercased
  - empty subscriptions do not match delivery

### Tool Metadata And Discovery

- `ToolFunctionMetadata.event_sources` should remain the authoritative discoverable declaration of what source kinds a tool can produce.
- `system/discover_tools` and `system/discover_agents` must continue to reflect runtime truth rather than drift into aspirational metadata only.
- Discovery should remain source-agnostic and not require task-daemon-specific knowledge.

### Provenance And Delivery Semantics

- Event delivery must preserve provenance continuity through `context_id`, `task_id`, and `message_id` where available.
- The runtime must continue to derive invocation scope from dispatch input so tracing and provenance remain attached to the originating event flow.
- Delivery semantics must be at least as strong as the current task-daemon behavior for not silently dropping events.
- If source checkpoints or cursors are involved, they must not advance before the system has reached the chosen success boundary for delivery.

### Generality

- The resulting model must be source-agnostic.
- The solution must not hardcode task-daemon assumptions into the general host runtime.
- Existing task-daemon behavior must keep working during and after the transition.

## Non-Goals

- Rewriting `docs/host-to-agent-event-delivery.md`
- Redesigning the agent-side `onDispatch` API
- Changing subscription matching semantics
- Solving every source integration in the first implementation
- Removing task-daemon before the general abstraction exists

## What A Strong Solution Should Do

A strong solution will introduce an explicit runtime abstraction for event-producing tools instead of trying to infer everything from metadata alone.

The implementation should make it clear:

- how a producer is registered
- how polling and webhook producers differ
- how producer lifecycle is managed by the host
- how emitted events become dispatch requests
- how checkpoints, retries, and fanout are handled
- how existing task-daemon behavior maps onto the new abstraction

In other words: metadata should advertise capability, but there still needs to be a runtime producer contract that actually performs intake.

## Design Questions The Solver Must Resolve Deliberately

These are not incidental details. They are the core of whether the implementation will be durable.

### 1. What is the runtime abstraction?

We need a deliberate answer to one of these shapes, or an equivalent better one:

- a new `EventProducer` trait alongside tool handlers
- an extension trait for host-managed tools that can emit events
- a companion bundle-level producer registration model
- a scheduler/ingress subsystem that is separate from the invocation tool registry but keyed off the same metadata

The answer must be explicit. `event_sources` alone is not enough.

### 2. How is routing determined?

Subscription matching uses schema version and source kind.

Actual dispatch also requires a routing key and agent capability alignment.

The solution needs a principled answer for where routing comes from:

- derived from source kind
- declared by the producer
- attached per event
- or another model that avoids hidden conventions

This must not devolve into fragile stringly-typed ad hoc mappings spread across binaries.

### 3. What is the success boundary for delivery?

The runtime must define what counts as success for:

- dispatch to one subscriber
- dispatch to many subscribers
- retries
- ack handling
- source checkpoint advancement

The current task-daemon implementation treats "no matching subscribers" as a delivery error to avoid advancing state and dropping the event. If the generalized system changes that rule, it must do so intentionally and document why.

### 4. How are polling and webhooks represented?

The documented vision includes both.

A good design should either:

- support both from the start, or
- support one explicitly while leaving a clean extension seam for the other

Do not build a polling-only abstraction that makes webhook adoption awkward or vice versa.

### 5. How does task-daemon fit afterward?

A strong answer should say whether task-daemon:

- becomes one producer implementation inside the common framework
- remains a separate binary that emits into the common dispatch boundary
- or gradually migrates behind the new abstraction

The migration story matters. We should not end up with two permanently divergent event systems.

## Acceptance Criteria

This issue is done when the following are true:

- There is a first-class host runtime abstraction for event-producing tools or producers.
- The abstraction is integrated with the existing event vocabulary:
  - `EventSourceKind`
  - `EventSubscription`
  - `AgentDispatchRequest`
  - `onDispatch`
- `event_sources` is operationalized, not just discoverable.
- At least one end-to-end producer path outside of pure metadata tests proves the model:
  - producer emits an event
  - host attaches delivery/provenance context
  - host matches subscribed agents
  - host dispatches to `/dispatch`
  - agent `onDispatch` receives the event
- Existing task-daemon dispatch behavior is preserved or intentionally migrated with no regression in delivery semantics.
- There are tests covering:
  - subscription matching through the generalized runtime path
  - fanout to multiple subscribers
  - no-subscriber behavior
  - failed subscriber delivery and retry or failure semantics
  - provenance threading through dispatch
  - discovery surfaces reflecting the real producer capability

## Recommended Test Shape

The solver should aim for a test pyramid that includes:

- unit tests for producer registration and metadata wiring
- unit tests for routing and subscription matching
- integration tests for host-managed delivery to a fixture agent using `onDispatch`
- regression tests that preserve task-daemon semantics where those semantics remain intentional

The most important end-to-end proof is a fixture where:

- a host-managed producer declares `event_sources`
- a fixture agent declares a matching subscription
- the host delivers a real `AgentDispatchRequest`
- the agent acknowledges via `onDispatch`

## Suggested Deliverable Shape

A strong implementation will likely require work across:

- `baml-rt-core`
- `baml-rt-tools`
- `baml-rt-a2a`
- the host/runner layer
- task-daemon integration or migration seams
- test fixtures and end-to-end coverage

It is acceptable to land this in phases, but the first phase should establish the correct abstraction rather than add another special case.

## References

- Vision doc: `docs/host-to-agent-event-delivery.md`
- Task-daemon behavior and delivery notes: `docs/task-daemon.md`
- Task-daemon event shape: `docs/task-daemon-event-contract.md`
- Subscription types and matching: `crates/baml-rt-core/src/event_subscription.rs`
- Tool metadata and discovery records: `crates/baml-rt-tools/src/tools.rs`
- Tool derive support for `event_sources`: `crates/baml-tool-derive/`
- System discovery tools: `crates/tools/system/src/`
- Dispatch transport and scope threading: `crates/baml-rt-a2a/src/a2a_transport.rs`

## Proposed Title

Make host-to-agent event delivery tool-native and source-agnostic so `event_sources` becomes an operational runtime capability
