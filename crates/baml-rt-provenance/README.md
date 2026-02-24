# baml-rt-provenance

`baml-rt-provenance` is the provenance subsystem for the runtime. It records runtime events, normalizes them into a PROV/A2A graph model, persists them in GraphQLite, and serves graph-backed reads for agent context and API/export consumers.

## Core model

- **Event input**: typed runtime events (`ProvEvent`, `ProvEventData`) for LLM calls, tool calls, task lifecycle, messages, and artifacts.
- **Normalization**: `normalize_event()` maps each event into a PROV document shape (entities, activities, agents, and typed relations).
- **Vocabulary**: all keys and semantic labels come from `baml-rt-vocabulary` (`a2a:*`, `prov:*`, and relation constants), with storage-safe key mapping for GraphQLite properties.
- **Graph identities**: stable derived IDs are used for graph nodes/activities to keep causality and linkage deterministic.

## Storage architecture

- **Concrete store**: `GraphqliteProvenanceStore` is the GraphQLite-backed implementation.
- **Builder**: `GraphqliteStoreBuilder` supports:
  - `file(path)` for file-backed per-build connections
  - `in_memory()` for shared in-memory execution
  - `backend(...)` for explicit backend selection
- **Write path**: events are persisted through parameterized Cypher (`cypher_with_params`) produced by `cypher_build`.
- **Read path**: context/tool queries are also parameterized Cypher and return typed runtime structures.

## Runtime interfaces

The store is consumed through narrow traits:

- `ProvenanceWriter`: append provenance events (`add_event`, `add_events`)
- `ProvenanceContextReader`: read context-construction data for runtime use
  - `context_messages(...)`
  - `conversation_context(...)`
- `ProvenanceQueryApi`: API-facing query surface with the same payload shapes
- `A2aGraphStore` (re-exported from `baml-rt-vocabulary`): task-subgraph operations used by A2A task/message/update persistence flows

`ProvenanceContextReader` is the strict context path for agent/runtime reads; `ProvenanceQueryApi` is the API query surface.

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
use baml_rt_provenance::{GraphqliteStoreBuilder, ProvenanceWriter};

let store = GraphqliteStoreBuilder::file("provenance.db").build()?;
store.add_event(event).await?;
```
