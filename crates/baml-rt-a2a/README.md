# baml-rt-a2a

Agent-to-agent (A2A) protocol support for the runtime.

## Responsibilities

- A2A request/response types and JSON-RPC helpers (e.g. `message.sendStream`, `tasks.list`).
- Task FSM: SUBMITTED → WORKING → INPUT_REQUIRED | COMPLETED | FAILED | …; stream chunks (status, message, artifact) drive task state. Status transitions are enforced only in `record_status_update`; `upsert` is for task create/merge (merge-preserve when status is `None`) and does not validate transitions.
- Transport and request handling for agent message flow; routing of resumed messages to pending `awaitInput` by task/context key.
- Agent builder wiring for runtime + bridge integration.

JavaScript agents use the A2A DSL provided by the runtime shim (`session`, `__chat_register({ run })`, `RunContext`, `emit.awaitInput`). See **task-lifecycle-demo** (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) for the reference implementation.
