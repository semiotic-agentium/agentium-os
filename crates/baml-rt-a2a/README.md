# baml-rt-a2a

Agent-to-agent (A2A) protocol support for the runtime.

## Responsibilities

- A2A request/response types and JSON-RPC helpers (e.g. `message.sendStream`, `tasks.list`).
- Task FSM: SUBMITTED → WORKING → INPUT_REQUIRED | COMPLETED | FAILED | …; stream chunks (status, message, artifact) drive task state. Status transitions are enforced only in `record_status_update`; `upsert` is for task create/merge (merge-preserve when status is `None`) and does not validate transitions.
- Transport and request handling for agent message flow; routing of resumed messages to pending `awaitInput` by task/context key.
- Agent builder wiring for runtime + bridge integration.
- Stream-first runtime boundary: handlers expose `handle_a2a_stream`; callers collect only at their own boundary when needed.
- Cross-turn streaming stabilization:
  - `TASK_STATE_INPUT_REQUIRED` is a non-final turn boundary.
  - HTTP `message.sendStream` emits an internal turn-end marker to terminate the current response stream while keeping the session alive for resume.
  - Continuation is driven by explicit `StreamCompletion` only.

JavaScript agents use the A2A DSL provided by the runtime shim (`session`, `__chat_register({ run })`, `RunContext`, `emit.awaitInput`). See **task-lifecycle-demo** (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) for the reference implementation.

## Task store and provenance (graph-native single-store design)

A2A task/state writes and provenance are backed by a **single concrete GraphQLite provenance store** in persistent mode. The transport composes narrow trait views over the same underlying store instance (DI), rather than maintaining a separate task-view authority.

- **Single concrete store:** `GraphqliteProvenanceStore` is instantiated once and projected into trait interfaces consumed by A2A (`TaskStoreBackend`, `ProvenanceWriter`, context reader facets).
- **Read path:** Conversation context is graph-backed and read from provenance for GraphQLite mode (message/tool/status history reconstructed from persisted graph events).
- **Write path:** Task/message/status/artifact mutations flow through A2A store adapters that emit matching provenance events; GraphQLite persistence remains the system of record.
- **Interface boundary:** A2A depends on trait contracts from shared vocabulary crates; graph labels/query shapes stay provenance-internal.
- **No implicit in-memory production fallback:** persistent GraphQLite mode is explicit; unsupported default combinations are rejected by builder wiring.

This keeps one causality graph for runtime behavior and avoids split-brain task/provenance state across multiple concrete stores.
