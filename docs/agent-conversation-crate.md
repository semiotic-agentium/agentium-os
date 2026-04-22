# `baml-rt-conversation`: agent-visible history and episodes

The `baml-rt-conversation` workspace crate holds **typed view models** for what agents and BAML see as conversation history, plus **pure** projection into `conversation_history` / `session_history` and episode-shaped JSON. It intentionally contains **no** SurrealDB, graph I/O, or `SurrealProvenanceStore`.

**Normative spec (requirements, ecosystem review, gap analysis, traceability):** [baml-rt-conversation-spec.md](baml-rt-conversation-spec.md).

## Where to look

| Concern | Location |
|--------|----------|
| Crate invariants and module map | `crates/baml-rt-conversation/src/lib.rs` |
| Rows from graph readers (messages, tools, session steps) | `crates/baml-rt-conversation/src/view.rs` |
| `PromptProjectionItem` / BAML text shaping | `crates/baml-rt-conversation/src/projection.rs` (uses `baml_rt_tools::prompt_projection`) |
| `Episode`, transcript, drift summaries | `crates/baml-rt-conversation/src/episode/mod.rs` |
| `assemble_session_history`, replay lines | `crates/baml-rt-conversation/src/session_history.rs` |
| Citation prefixing, `render_episode` | `crates/baml-rt-conversation/src/render.rs` |
| Status/artifact timeline rows (merged with conv) | `crates/baml-rt-conversation/src/timeline.rs` |

## What stays in `baml-rt-provenance`

- Surreal store, normalizer, graph export, **async** `EpisodeReader`, and traits `ProvenanceContextReader` / `ProvenanceQueryApi` (`crates/baml-rt-provenance/src/store.rs` and `surreal_store/`).
- **Storage and effects** types. Conversation **row** types are **not** re-exported from the provenance crate root; import them from `baml_rt_conversation::view`.

## HTTP and BAML alignment

- API DTOs for **conversation history** and **episode snapshots** map from `baml_rt_conversation` in `crates/baml-rt-api/src/conversation_history.rs` and `crates/baml-rt-api/src/episode.rs`.
- Policy alignment (line caps, projection options) is shared via `baml_rt_tools::prompt_projection` (e.g. `episode_session_history_projection_options` for episode `session_history`).

## Related docs

- [How to write agents](how-to-write-agents.md) (entrypoints, tools, `StructuredReply`).
- [Citable history and checked citations](citable-history-and-checked-citations.md) (grounding and refs).
