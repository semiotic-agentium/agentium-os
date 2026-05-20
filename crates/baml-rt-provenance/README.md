# baml-rt-provenance

`baml-rt-provenance` is the provenance subsystem for the runtime. It records runtime events, normalizes them into a PROV/A2A graph model, persists them in SurrealDB, and serves graph-backed reads for agent context and API/export consumers.

## Core model

- **Event input**: typed runtime events (`ProvEvent`, `ProvEventData`) for LLM calls, tool calls, task lifecycle, messages, and artifacts.
- **Normalization**: `normalize_event()` maps each event into a PROV document shape (entities, activities, agents, and typed relations).
- **Vocabulary**: all keys and semantic labels come from `baml-rt-vocabulary` (`a2a:*`, `prov:*`, and relation constants), with storage-safe key mapping for SurrealDB properties.
- **Graph identities**: stable derived IDs are used for graph nodes/activities to keep causality and linkage deterministic.

## Storage architecture

- **Concrete store**: `SurrealProvenanceStore` is the SurrealDB-backed implementation.
- **Builder**: `SurrealStoreBuilder` supports:
  - `file(path)` for file-backed SurrealKV storage
  - `in_memory_isolated()` for isolated in-memory execution
  - `in_memory_shared()` for shared in-memory execution
- **Write path**: events are persisted through SurrealQL queries with parameterized bindings.
- **Read path**: context/tool queries use SurrealQL and return typed runtime structures.
- **Ref registry**: `history_ref_registry` and `session_ref_counter` persist stable `#N` indices; `archive_body` persists `@N` bodies. In-process `RefTable` / `ContextRefTables` and `TaskUpdateBroadcaster` are **caches or live fan-out only** — rebuild via `hydrate_ref_table` / `prepare_ref_table_for_projection` after restart or on cache miss.

## Runtime interfaces

The store is consumed through narrow traits:

- `ProvenanceWriter`: append provenance events (`add_event`, `add_events`)
- `ProvenanceContextReader`: read context-construction data for runtime use
  - `context_messages(...)`
  - `conversation_context(...)`
- `ProvenanceQueryApi`: API-facing query surface with the same payload shapes
- `TaskGraphReader`: graph-only A2A task/message/status surface (no relational shadow tables)

`ProvenanceContextReader` is the strict context path for agent/runtime reads; `ProvenanceQueryApi` is the API query surface. SurrealDB uses MVCC — callers await completed writes before agent-context reads; no separate in-process write worker is required.

## Graph export and rendering

`graph_export` reads persisted graph subgraphs and emits `ExportedGraph`:

- export scopes:
  - `export_by_context(context_id)`
  - `export_by_task(task_id)`
- renderers:
  - Mermaid sequence (`graph_export::sequence`)
  - Graphviz DOT (`graph_export::dot`)
  - JSON (`graph_export::json`)
- simplification:
  - `graph_export::simplify` removes structural noise and keeps sequence-relevant flow

Sequence rendering expresses conversation and execution flow directly:

- user/agent message arrows
- LLM reasoning notes
- tool request/response arrows
- task status transition notes
- artifact generation notes

## Interceptors and subscribers

- `ProvenanceInterceptor` emits provenance around LLM and tool execution.
- Bus/effect subscribers translate runtime bus/effect events into provenance events.
- Tool indexing (`tool_index`) writes tool metadata into the graph for discovery/context usage.

## Typical construction

```rust
use baml_rt_provenance::{SurrealStoreBuilder, ProvenanceWriter};

let store = SurrealStoreBuilder::file("provenance_data").build().await?;
store.add_event(event).await?;
```
