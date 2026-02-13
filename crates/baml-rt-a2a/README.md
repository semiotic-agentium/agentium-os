# baml-rt-a2a

Agent-to-agent (A2A) protocol support for the runtime.

## Responsibilities

- A2A request/response types and JSON-RPC helpers (e.g. `message.sendStream`, `tasks.list`).
- Task FSM: SUBMITTED → WORKING → INPUT_REQUIRED | COMPLETED | FAILED | …; stream chunks (status, message, artifact) drive task state. Status transitions are enforced only in `record_status_update`; `upsert` is for task create/merge (merge-preserve when status is `None`) and does not validate transitions.
- Transport and request handling for agent message flow; routing of resumed messages to pending `awaitInput` by task/context key.
- Agent builder wiring for runtime + bridge integration.

JavaScript agents use the A2A DSL provided by the runtime shim (`session`, `__chat_register({ run })`, `RunContext`, `emit.awaitInput`). See **task-lifecycle-demo** (`tests/fixtures/agents/task-lifecycle-demo/src/index.ts`) for the reference implementation.

## Event Stream (A2A)

Streaming responses may include **event chunks** alongside message/task/status/artifact chunks. These are emitted by the host runtime (Rust) to provide Pi-style lifecycle signals without requiring JS changes.

Event chunks look like:

```json
{ "event": { "type": "message_start", "contextId": "...", "taskId": "...", "messageId": "...", "source": "runtime" } }
```

Currently emitted for `message.sendStream`:
- `agent_start`, `agent_end` (once per stream)
- `turn_start`, `turn_end` (per message chunk)
- `message_start`, `message_end` (per message chunk)
- `message_update` (only when partial message fields are present)
- `tool_execution_start`, `tool_execution_update`, `tool_execution_end` (from tool-session wrapper)

These events are additive and can be consumed by UIs or loggers to build rich, auditable timelines.

### Debugging

For quick inspection of streams, use the small CLI:

```bash
cargo run -p baml-rt-a2a --bin a2a-event-dump < stream.jsonl
```
