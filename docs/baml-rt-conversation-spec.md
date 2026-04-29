# baml-rt-conversation — normative notes

This document anchors the contracts referenced from [`agent-conversation-crate.md`](agent-conversation-crate.md) and [`crates/baml-rt-conversation/src/lib.rs`](../crates/baml-rt-conversation/src/lib.rs). The crate is **pure** (no Surreal / graph I/O); producers live in `baml-rt-provenance` and consumers include BAML tags (`ctx.tags['conversation_history']`) and the HTTP API.

## Three boundaries (normative)

These separate **what happened** in the graph from **how the model is guided** in a given phase, and from **ephemeral ref labels** in the prompt.

| Boundary | Owns | Must not |
|----------|------|----------|
| **CanonicalHistory** | Ordered, graph-derived view of `Message` / `ToolCall` / `SessionStep` rows (via `ProvenanceConversationContextItem` → `PromptProjectionItem`). | Contain hand-written FSM paragraphs, `[ACT]` / `[CONTINUE]` preambles, or merge heuristics that invent or drop activities. |
| **PhaseOverlay** (session step executors) | Transient, phase-specific *prompt* material (if any) outside the parent function’s `prompt_template` from IR. | Be concatenated into `ctx.tags['conversation_history']` or be mistaken for canonical `system` history. Today, generated phase functions use an **empty** preamble so FSM is expressed by **narrowed return types** and BAML `@@description` / class docs, not by injected prose in the `prompt` string. |
| **StableHistoryRefResolver** | `RefTable` mapping from `(a2a_activity_anchor, source)` to `#N` for `Message` and `ToolCall` lines; monotonic `insert` for new `@N` archives. | Advance `#N` on a full-graph re-read when the same activities are projected again — [`RefTable::insert_history`](../../crates/baml-rt-tools/src/archive_refs.rs) is idempotent per that key. |

## Boundary map (live A2A)

1. **Write path** — Provenance events become graph nodes; `a2a_event_order` and edges define truth.
2. **Read path** — [`conversation_context_filtered`](../crates/baml-rt-provenance/src/surreal_store/context_reader.rs) returns rows ordered by `event_order ASC, node_id ASC` (total order when `event_order` ties).
3. **Map** — [`provenance_item_to_projection_item`](../crates/baml-rt-conversation/src/projection.rs) → [`PromptProjectionItem`](../crates/baml-rt-tools/src/prompt_projection.rs).
4. **Render** — [`project_prompt_context`](../crates/baml-rt-tools/src/prompt_projection.rs) assigns `#N` via idempotent `RefTable::insert_history` (stable per activity + source).
5. **Provider** — [`ProjectingConversationContextProvider`](../crates/baml-rt-a2a/src/a2a_transport.rs) → `ctx.tags['conversation_history']` in [`BamlExecutor`](../crates/baml-rt-quickjs/src/baml_execution.rs).
6. **Intra-turn** — [`baml/intra_turn.rs`](../crates/baml-rt-quickjs/src/baml/intra_turn.rs) may merge a loop-local supplement with the provider; dedup is by full JSON line equality (stable refs make re-reads match).
7. **Phase executors (codegen)** — [`session_from_ir/mod.rs`](../crates/baml-rt-builder/src/builder/baml_gen/session_from_ir/mod.rs) generates per-phase BAML **without** long `[OPEN]` / `[ACT]` / `[CONTINUE]` preambles; IR `prompt_template` is the only inlined task text in the default path.
8. **Citation drift** — [`compute_citation_drift_section`](../crates/baml-rt-provenance/src/effect_subscriber.rs) re-runs `project_prompt_context` on the live `RefTable`; idempotent history refs keep `#N` aligned with the last build.

## Non-negotiable invariants

- **Temporal order (live):** rows from the store are ordered with a deterministic tie-break (`node_id` after `event_order`). Episode merge uses a stronger key in [`episode/reader.rs`](../crates/baml-rt-provenance/src/episode/reader.rs); both avoid unstable ordering when ordinals collide.
- **Canonical purity:** `conversation_history` JSON is graph-faithful; FSM *phase* instructions are not mixed into that array as if they were user/assistant events.
- **1:1 activity → history lines (for Message/ToolCall):** one `Message` or `ToolCall` activity yields one primary `#N` line each pass; if the same user text appears three times with three `#N` values, the graph has three `Message` activities (or a transport bug) — the projector does not dedupe.
- **Stable `#N` for reprojection:** repeated `project_prompt_context` on the same items with the same `RefTable` produces identical `conversation_history` line content for those activities.
- **Merge / supplement:** step-executor merge uses graph-backed line objects; with stable refs, provider and supplement lines match when they represent the same activity.

## Known fault lines (where bugs surface)

- **Provenance write:** duplicate `Message` nodes or bad `event_order` → wrong order or repeated lines in the graph; fix the write path, not the projector.
- **Ref churn (mitigated):** previously, `insert_history` allocated a new `#N` on every full pass, breaking intra-turn `Value` equality; now idempotent per `(activity_anchor, source)`.
- **Builder preambles (mitigated for generated phase tools):** long `[ACT]` / `[CONTINUE]` blocks in the `prompt` string conflated **PhaseOverlay** with the task template; removed from [`session_from_ir/mod.rs`](../crates/baml-rt-builder/src/builder/baml_gen/session_from_ir/mod.rs) — hand-written BAML in agent repos may still document FSM in `@@description` or comments.
- **Intra-turn async:** `LlmCompleted` can lag; the step executor supplement exists until the graph catches up; this is not non-monotonic *provenance*, but a composite read until sync.

## R9 — Prefix cacheability (append-only before compaction)

The default **projection** appends new rows; it does not rewrite earlier `conversation_history` lines on every turn. Compaction or windowing (if added) must be an explicit policy change. This keeps **LLM prefix caches** stable when the left prefix of the prompt is intentionally reused.

## End-to-end pipeline (A2A + episode)

1. **Graph / store** produces [`ProvenanceConversationContextItem`](../crates/baml-rt-conversation/src/view.rs) rows.
2. **Map** with [`provenance_item_to_projection_item`](../crates/baml-rt-conversation/src/projection.rs) → [`PromptProjectionItem`](../crates/baml-rt-tools/src/prompt_projection.rs).
3. **Render** with [`project_prompt_context`](../crates/baml-rt-tools/src/prompt_projection.rs) (live A2A: `ProjectingConversationContextProvider` in `crates/baml-rt-a2a/src/a2a_transport.rs`) or [`assemble_session_history`](../crates/baml-rt-conversation/src/session_history.rs) (episode). Live rendering uses a per-context [`RefTable`](../crates/baml-rt-tools/src/archive_refs.rs) for `#N` / `@N`; `insert_history` is idempotent per `(activity_anchor, source)` so reprojection is stable. Episode local tables use [`episode_session_history_projection_options`](../crates/baml-rt-tools/src/prompt_projection.rs).

`crates/baml-rt-conversation` re-exports the alignment types (`ProjectionRenderOptions`, `ProjectedLineRole`, `project_projection_item_to_rows`, `episode_session_history_projection_options`) for importers that prefer a single crate path.

## Live `conversation_history` JSON

Implemented by [`project_prompt_context`](../crates/baml-rt-tools/src/prompt_projection.rs):

- Each element is at least `{ "role", "content" }`.
- **Message**-sourced rows may add `"citations": string[]` when the graph provided non-empty CITED refs (wire strings: `#N`, `@K`, line ranges, negation as `!#N` / `!@K`, per `Citation`).
- **Session** steps: each graph row is projected independently. If a view must not appear twice, fix the write path; the projector does not suppress repeated `SendDone` / `Read` lines.

## Episode `session_history`

[`assemble_session_history`](../crates/baml-rt-conversation/src/session_history.rs) must use the **same** [`project_projection_item_to_rows`](../crates/baml-rt-tools/src/prompt_projection.rs) + [`ProjectionRenderOptions`] rules as `project_prompt_context` (provenance uses [`episode_session_history_projection_options`](../../crates/baml-rt-tools/src/prompt_projection.rs)).

- [`SessionHistoryLine`](../crates/baml-rt-conversation/src/episode/mod.rs) includes `citations` (episode-prefixed) on message rows; tool/system lines use an empty list.
- The merged timeline (prior + in-task + status + artifacts) is defined in the provenance [`EpisodeReader`](../crates/baml-rt-provenance/src/episode/reader.rs) — not the task-only `transcript` slice alone.

## `read_replay_lines` and `send_done_replay_payload`

Graph rows may carry:

- `read_replay_lines` on `SearchRead` / `PageRead` — pre-hydrated text; the projection must prefer this over calling the archive reader when non-empty.
- `send_done_replay_payload` on `SendDone` — optional JSON from the graph for **ref-table seeding and hydration**; it is **not** rendered as `conversation_history` or transcript content. `SendDone` projects as a **summary + read-guidance** line only; archive lines appear on explicit `SearchRead` / `PageRead` rows.

Types: [`SessionStepContent`](../crates/baml-rt-conversation/src/view.rs) and [`SessionStepPayload`](../crates/baml-rt-tools/src/prompt_projection.rs).

## Merged timeline ordering

The episode reader sorts merged status + conversation + artifacts with a **total** key (timestamp / event order, kind, prior-vs-task, activity anchor) so equal timestamps cannot reorder non-deterministically. See `merged.sort_by_cached_key` in [`reader.rs`](../crates/baml-rt-provenance/src/episode/reader.rs).

## Traceability

| Topic | Location |
|--------|----------|
| **Regression (snapshots)** | `crates/baml-rt-a2a/tests/conversation_history_snapshot.rs` — insta JSON: [`project_prompt_context`](../../crates/baml-rt-tools/src/prompt_projection.rs) (default options, same as runtime) with a **stub** [`ToolRegistry`](../../crates/baml-rt-tools/src/tools.rs) registering `system/discover_agents`; message, execute-phase **ToolCall+ToolResult**, **SessionStep** (Open, SendDone, SearchRead, PageRead), **`ContextRefTables` + `ArchiveReader`**. `crates/baml-rt-provenance/tests/episode_reader_integration.rs` — `Episode::session_history`. `INSTA_UPDATE=1` when the shape is intentionally changed. |
| View rows | `crates/baml-rt-conversation/src/view.rs` |
| Provenance → `PromptProjectionItem` | `crates/baml-rt-conversation/src/projection.rs` |
| Tag JSON (`conversation_history`) | `crates/baml-rt-tools/src/prompt_projection.rs` |
| Episode `session_history` | `crates/baml-rt-conversation/src/session_history.rs` |
| Episode merge + sort | `crates/baml-rt-provenance/src/episode/reader.rs` |
| API DTOs | `crates/baml-rt-api/src/episode.rs`, `conversation_history.rs` |

## Test gaps and enforced checks

- **Idempotent history refs** — `baml-rt-tools` unit tests: `RefTable::insert_history` and `project_prompt_context` re-run (see `repeat_projection_byte_identical_when_graph_unchanged`).
- **“One user utterance → one row” (product)** — still host/store responsibility; **faithful** projection: three `Message` activities with the same text → three `#N` lines ([Live `conversation_history` JSON](#live-conversation_history-json)). Add integration tests on the **write path** to catch duplicate `Message` emissions.
- **Live ordering** — `ORDER BY event_order ASC, node_id ASC` in [`context_reader.rs`](../crates/baml-rt-provenance/src/surreal_store/context_reader.rs) for deterministic ties; add a regression test if the store is ever found to assign duplicate `event_order` to distinct nodes in one context.
- **Phase preambles** — generated phase BAML in **builder** uses an empty preamble; `baml-rt-builder` test `phase_executor_prompt_body_uses_empty_preamble_for_full_cutover`. Re-run `regen_fixtures` / `baml-rt-builder` to refresh committed `agents/**/baml_src/_baml_runtime.baml` when the generator changes. Hand-written agent BAML is unchanged by this.
- **High `@N` for new archives** — new archive rows still advance the shared per-context counter; only **history** lines are idempotent per activity.

**What existing snapshots still primarily cover:** JSON shape, `SessionStep` `SendDone` two-liner, `ArchiveReader` wiring, episode `session_history` goldens, step-executor merged history growth.
