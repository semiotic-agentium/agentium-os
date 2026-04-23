# baml-rt-conversation — normative notes

This document anchors the contracts referenced from [`agent-conversation-crate.md`](agent-conversation-crate.md) and [`crates/baml-rt-conversation/src/lib.rs`](../crates/baml-rt-conversation/src/lib.rs). The crate is **pure** (no Surreal / graph I/O); producers live in `baml-rt-provenance` and consumers include BAML tags (`ctx.tags['conversation_history']`) and the HTTP API.

## R9 — Prefix cacheability (append-only before compaction)

The default **projection** appends new rows; it does not rewrite earlier `conversation_history` lines on every turn. Compaction or windowing (if added) must be an explicit policy change. This keeps **LLM prefix caches** stable when the left prefix of the prompt is intentionally reused.

## End-to-end pipeline (A2A + episode)

1. **Graph / store** produces [`ProvenanceConversationContextItem`](../crates/baml-rt-conversation/src/view.rs) rows.
2. **Map** with [`provenance_item_to_projection_item`](../crates/baml-rt-conversation/src/projection.rs) → [`PromptProjectionItem`](../crates/baml-rt-tools/src/prompt_projection.rs).
3. **Render** with [`project_prompt_context`](../crates/baml-rt-tools/src/prompt_projection.rs) (live A2A: `ProjectingConversationContextProvider` in `crates/baml-rt-a2a/src/a2a_transport.rs`) or [`assemble_session_history`](../crates/baml-rt-conversation/src/session_history.rs) (episode) using shared [`InlineProjectionState`](../crates/baml-rt-tools/src/prompt_projection.rs) and, for session history, [`episode_session_history_projection_options`](../crates/baml-rt-tools/src/prompt_projection.rs).

`crates/baml-rt-conversation` re-exports the alignment types (`InlineProjectionState`, `ProjectionRenderOptions`, `ProjectedLineRole`, `project_projection_item_to_rows`, `episode_session_history_projection_options`) for importers that prefer a single crate path.

## Live `conversation_history` JSON

Implemented by [`project_prompt_context`](../crates/baml-rt-tools/src/prompt_projection.rs):

- Each element is at least `{ "role", "content" }`.
- **Message**-sourced rows may add `"citations": string[]` when the graph provided non-empty CITED refs (wire strings: `#N`, `@K`, line ranges, negation as `!#N` / `!@K`, per `Citation`).
- **Session** steps share **cross-item** inline state via [`InlineProjectionState`](../crates/baml-rt-tools/src/prompt_projection.rs): `SendDone` and `SearchRead` / `PageRead` de-duplicate repeated archive **views** so archive bodies are not re-inlined when the model repeats the same read.

## Episode `session_history`

[`assemble_session_history`](../crates/baml-rt-conversation/src/session_history.rs) must use the **same** `InlineProjectionState` + [`project_projection_item_to_rows`](../crates/baml-rt-tools/src/prompt_projection.rs) sequence as `project_prompt_context`, with the same [`ProjectionRenderOptions`] (provenance uses [`episode_session_history_projection_options`](../../crates/baml-rt-tools/src/prompt_projection.rs)).

- [`SessionHistoryLine`](../crates/baml-rt-conversation/src/episode/mod.rs) includes `citations` (episode-prefixed) on message rows; tool/system lines use an empty list.
- The merged timeline (prior + in-task + status + artifacts) is defined in the provenance [`EpisodeReader`](../crates/baml-rt-provenance/src/episode/reader.rs) — not the task-only `transcript` slice alone.

## `read_replay_lines` and `send_done_replay_payload`

Graph rows may carry:

- `read_replay_lines` on `SearchRead` / `PageRead` — pre-hydrated text; the projection must prefer this over calling the archive reader when non-empty, and must still update read-view dedup keys consistently with the archive path.
- `send_done_replay_payload` on `SendDone` — JSON replay for the read body; formatted with the same `send_done` line cap as archive reads.

Types: [`SessionStepContent`](../crates/baml-rt-conversation/src/view.rs) and [`SessionStepPayload`](../crates/baml-rt-tools/src/prompt_projection.rs).

## Merged timeline ordering

The episode reader sorts merged status + conversation + artifacts with a **total** key (timestamp / event order, kind, prior-vs-task, activity anchor) so equal timestamps cannot reorder non-deterministically. See `merged.sort_by_cached_key` in [`reader.rs`](../crates/baml-rt-provenance/src/episode/reader.rs).

## Traceability

| Topic | Location |
|--------|----------|
| View rows | `crates/baml-rt-conversation/src/view.rs` |
| Provenance → `PromptProjectionItem` | `crates/baml-rt-conversation/src/projection.rs` |
| Tag JSON + dedup | `crates/baml-rt-tools/src/prompt_projection.rs` |
| Episode `session_history` | `crates/baml-rt-conversation/src/session_history.rs` |
| Episode merge + sort | `crates/baml-rt-provenance/src/episode/reader.rs` |
| API DTOs | `crates/baml-rt-api/src/episode.rs`, `conversation_history.rs` |
