# baml-rt-a2a

Agent-to-agent (A2A) protocol support for the runtime.

## Responsibilities

- A2A request/response types and JSON-RPC helpers (e.g. `message.sendStream`, `tasks.list`).
- Task FSM: SUBMITTED → WORKING → INPUT_REQUIRED | COMPLETED | FAILED | …; stream chunks (status, message, artifact) drive task state. Status transitions are enforced only in `record_status_update`; `upsert` is for task create/merge (merge-preserve when status is `None`) and does not validate transitions.
- Transport and request handling for agent message flow; routing of resumed messages to pending `awaitInput` by task/context key.
- Agent builder wiring for runtime + bridge integration.
- Stream-first runtime boundary: handlers expose `handle_a2a_stream`; callers collect only at their own boundary when needed.
- Cross-turn streaming stabilization: `TASK_STATE_INPUT_REQUIRED` is treated as a live wait state, allowing resume without deadlocking the runtime bridge.

JavaScript agents use the A2A DSL provided by the runtime shim (`session`, `__chat_register({ run })`, `RunContext`, `emit.awaitInput`). See **task-lifecycle-demo** (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) for the reference implementation.

## Task store and provenance (unified design)

The A2A task store and conversation context share a **single source of truth**: the in-memory task store (materialized view). Provenance (e.g. GraphQLite) is a **write-only** audit log; the runtime never reads conversation from it.

- **Read path:** Task state (`get`, `list`, `cancel`) and **conversation context** both come from the same backend. `TaskStoreBackend` extends `ConversationContextSource`: `conversation_context(context_id, limit)` returns messages from task histories in that context. The runtime’s conversation provider uses this, so there is no separate “provenance read” for context.
- **Write path:** All mutations (upsert, insert_message, apply_task_delta, record_status_update, record_artifact_update, cancel) go through the store. When a `ProvenanceWriter` is present, the store emits corresponding `ProvEvent`s after accepting the update (provenance lags the view; no path updates only the view or only provenance).
- **Invariant:** No code path updates the in-memory view without also emitting the matching provenance events when a writer is configured. Cancel is included: `ProvenanceTaskStore::cancel` emits `task_status_changed(..., CANCELED)`.

Custom backends must implement `ConversationContextSource` (e.g. by delegating to an inner store or returning an empty list). The agent builder always sets the conversation context provider from the task store; provenance interceptors (LLM/tool events) are registered only when a writer is provided.
