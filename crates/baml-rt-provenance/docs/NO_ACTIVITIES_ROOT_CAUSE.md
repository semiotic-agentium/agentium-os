# Root Cause: No Activities in Mermaid Sequence Diagram

## Symptom

The mermaid endpoint (`/mermaid/context/{context_id}`) returns a diagram with only `participant discover_agents` (a tool) but no User, agents, or activity arrows.

## Root Cause Chain

### 1. Activity emission requires `agent_for_node`

In `sequence.rs`, we only emit activity arrows for nodes that appear in `agent_for_node`:

```rust
if let Some(agent) = agent_for_node.get(&node.id) {
    emit_node(...);
}
```

`agent_for_node` is built by `resolve_agent_package_for_node_indices`, which returns `Some` only when:

- The node (or its parent activity) is in `activity_to_agent`
- The agent node has `a2a:archive_path`

### 2. `activity_to_agent` requires AgentRuntimeInstance in the graph

`ResolutionMaps::build` populates `activity_to_agent` from edges:

- `activity -[WAS_EXECUTED_BY]-> AgentRuntimeInstance`
- `activity -[WAS_INVOKED_BY]-> AgentRuntimeInstance`

For LlmCall/ToolCall, the chain is:

- `TaskExecution -[WAS_INVOKED_BY]-> LlmCall` (TaskExecution invokes LlmCall)
- `TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance` (TaskExecution executed by agent)

So we need **AgentRuntimeInstance** in the exported graph. `RunnerRuntimeInstance` does not count (we only match `AgentRuntimeInstance` label).

### 3. Export returns only nodes with SCOPED_TO

The export query is:

```sql
MATCH (ctx:Context {id: '...'})-[:SCOPED_TO]->(a), (a)-[r]->(b), (ctx)-[:SCOPED_TO]->(b)
RETURN ...
```

Both `a` and `b` must have `SCOPED_TO` from the Context. If AgentRuntimeInstance does not have SCOPED_TO, we get no edges involving it, so `activity_to_agent` stays empty.

### 4. SCOPED_TO is written only from the normalized document

In the store write path:

```rust
let scoped_node_ids = normalized.document
    .entities().map(...)
    .chain(normalized.document.activities().map(...))
    .chain(normalized.document.agents().map(...))
    .filter(|(_, label)| !SCOPE_EXEMPT_LABELS.contains(&label))
    .map(|(id, _)| id)
    .collect();
```

SCOPED_TO is created only for nodes that appear in the **current event's** normalized document. Each event is processed separately; there is no cross-event aggregation.

### 5. AgentRuntimeInstance is created only by AgentBooted

AgentRuntimeInstance (with `archive_path`) is inserted into the document only when processing `ProvEventData::AgentBooted`. `TaskExecutionStarted` and `get_agent_runtime_instance` do **not** create it; they only reference an existing agent or add to `agent_labels` (which does not create nodes).

### 6. The failing scenario: AgentBooted has no context

**AgentBooted does not have a context.** It is a global event—the agent is booted once, before any conversation. Semantically, it is not scoped to a conversation context.

The current write path adds SCOPED_TO only from the **current event's** context_id. For AgentBooted, the runner may pass a generated context_id (or similar), but that context is not a real conversation context. No conversation will ever export by that context. So:

- AgentRuntimeInstance is created by AgentBooted
- If AgentBooted has no meaningful context, we either skip SCOPED_TO for its nodes, or we add SCOPED_TO to a phantom context no one exports by
- When we export by the **conversation** context, AgentRuntimeInstance is not reachable
- Result: AgentRuntimeInstance missing from export → no `activity_to_agent` → no activity arrows
- Tools still appear (ToolCall nodes have SCOPED_TO from the conversation context)

---

## Why Tests Do Not Cover This

### 1. Integration tests always emit AgentBooted first

Every sequence integration test (`file_backed_export_renders_expected_sequence_flow`, `message_received_global_with_agent_id_renders_initial_user_message`, `coordinator_flow_shows_both_user_and_delegated_messages`, etc.) does:

```rust
store.add_event(ProvEvent::agent_booted(context_id, agent_id, ...)).await?;
// then task_exists, task_execution_started, message_received, llm_call, tool_call, ...
```

So AgentRuntimeInstance is always present with SCOPED_TO. We never exercise the "no AgentBooted" path.

### 2. No SCOPED_TO coverage assertion

We do not assert that the write path produces SCOPED_TO for all expected nodes. A test could:

- Add a full event sequence
- Run `MATCH (ctx:Context {id: $id})-[:SCOPED_TO]->(n) RETURN count(n)`
- Assert the count matches the expected entities + activities + agents (excluding SCOPE_EXEMPT)

### 3. Unit tests bypass the write path

Sequence unit tests use `graph(nodes, edges)` directly. They never run:

`add_event → normalizer → store write → SCOPED_TO → export → parse → simplify → render`

So they cannot catch SCOPED_TO or export-scoping bugs.

### 4. No "coordinator without boot" scenario

We have no test that:

- Adds MessageReceived, LlmCall, ToolCall **without** AgentBooted
- Asserts the diagram behavior (e.g. tools appear but no agent arrows, or a clear failure mode)

---

## Recommended Test Additions

1. **`scoped_to_covers_all_document_nodes`** – Add events, run SurrealQL to count SCOPED_TO edges from Context, assert count ≥ expected (entities + activities + agents, minus exempt).

2. **`sequence_without_agent_booted_shows_tools_only`** – Add MessageReceived, LlmCall, ToolCall, MessageSent **without** AgentBooted. Export and render. Assert:
   - Tools appear as participants
   - No agents in participants
   - No User→Agent or Agent→User arrows (or document the current behavior)

3. **`export_returns_agent_runtime_instance_when_booted`** – Add AgentBooted + TaskExecutionStarted + LlmCallCompleted. Export. Assert graph contains at least one AgentRuntimeInstance node and at least one WAS_EXECUTED_BY edge to it.

---

## Fix (implemented)

**Export query change:** Only require the source node (a) to be scoped; the target (b) is reached via the relation. AgentRuntimeInstance has no context, but it is reachable via MessageProcessing -[WAS_EXECUTED_BY]-> AgentRuntimeInstance and TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance.

```sql
-- Before: both a and b must be scoped
MATCH (ctx)-[:SCOPED_TO]->(a), (a)-[r]->(b), (ctx)-[:SCOPED_TO]->(b)

-- After: only a must be scoped
MATCH (ctx)-[:SCOPED_TO]->(a), (a)-[r]->(b)
```

**AgentBooted has no context by design:** `ProvEvent::AgentBooted` is a context-free variant; `agent_booted()` no longer takes `context_id`.

---

## Design principle: traverse, don't denormalize

Since we can traverse the graph (e.g. MessageProcessing -[WAS_EXECUTED_BY]-> AgentRuntimeInstance, Task -[WAS_CREATED_BY]-> TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance), we do not need to write `a2a:agent_id` on every node. Agent attribution can be derived by following edges. Redundant `agent_id` properties are denormalization that can be removed in favour of traversal.

**Graph enrichment:** After parsing the export result, `enrich_derived_properties` derives `task_id`, `context_id`, and `agent_id` by id parsing and edge traversal. Nodes that lack these properties get them filled in before `filter_scope` runs.

**Task→agent identity (head pointer):** `get_task_agent_id` resolves via a single hop: `Task` -[`WAS_LAST_EXECUTED_BY`]-> `AgentRuntimeInstance` → `a2a_agent_id`. That head pointer is written by `TaskExecutionStarted` (or defense-in-depth repoint when the first non-nil agent attribution lands in the same normalize batch). Scope-establishment paths (`withTask`, `handle_dispatch`, A2A chat bootstrap) must emit binding via `TaskAgentBinding` / `bind_task_executing_agent` — see `task_agent_binding.rs` and `docs/host-to-agent-event-delivery.md`.

**context_id and task_id retained:** Message and ToolCall must have `a2a:context_id` for `context_messages` and `conversation_context` read queries. Task-scoped nodes need `a2a:task_id` for `export_by_task`. Enrichment fills gaps for nodes reached by traversal (e.g. AgentRuntimeInstance).
