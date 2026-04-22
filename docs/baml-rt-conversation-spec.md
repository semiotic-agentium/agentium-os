# `baml-rt-conversation`: normative specification

This document is the **engineering contract** for the [`baml-rt-conversation`](../crates/baml-rt-conversation) workspace crate. The short overview remains [agent-conversation-crate.md](agent-conversation-crate.md); for graph storage invariants, see [store traits](../crates/baml-rt-provenance/src/store.rs) and [citable history](citable-history-and-checked-citations.md).

---

## A. Context and position in the system

`baml-rt-conversation` is the **pure** layer: typed view model, projection into BAML `conversation_history`, episode transcript DTOs, `session_history` line assembly, rendering, and merged status/artifact timelines. It does **not** read from SurrealDB or own persistence.

```mermaid
flowchart LR
  subgraph prov [baml_rt_provenance]
    graph[Graph_rows_Surreal]
    traits[ProvenanceContextReader_and_QueryApi]
    reader[EpisodeReader_async]
  end
  subgraph conv [baml_rt_conversation]
    view[view_and_context_items]
    proj[projection]
    ep[episode_transcript]
    sh[session_history]
    render[render_episode]
  end
  subgraph tools [baml_rt_tools]
    pp[prompt_projection]
  end
  subgraph surfaces [Consumers]
    baml[BAML_conversation_history]
    http[HTTP_DTOs]
  end
  graph --> view
  traits --> graph
  reader --> ep
  view --> proj
  proj --> pp
  ep --> sh
  sh --> pp
  render --> baml
  proj --> baml
  ep --> http
  conv -.->|no_surreal_in_crate| surfaces
```

**Boundary (normative):** No `surrealdb` dependency, no `SurrealProvenanceStore` import, and no hidden network or filesystem I/O in the default modules of this crate. Callers pass in data and closures (e.g. archive read backed by a pre-built `RefTable`).

---

## B. Normative requirements

| ID | Requirement |
|----|-------------|
| **R1** | **Graph inputs only.** View rows ([`ProvenanceConversationContextItem`](../crates/baml-rt-conversation/src/view.rs) and related types) are **already** graph-reconstructed by `baml-rt-provenance` (or tests). This crate does not repair missing edges, backfill, or “fix” write-path bugs. |
| **R2** | **Projection isomorphism.** Live BAML `conversation_history` and episode [`SessionHistoryLine`](../crates/baml-rt-conversation/src/episode/mod.rs) materialization use the same policy via [`baml_rt_tools::prompt_projection::ProjectionRenderOptions`](../crates/baml-rt-tools/src/prompt_projection.rs). Episode paths **must** use [`episode_session_history_projection_options()`](../crates/baml-rt-tools/src/prompt_projection.rs) (or an explicitly reviewed alternative) when calling [`assemble_session_history`](../crates/baml-rt-conversation/src/session_history.rs). |
| **R3** | **Documented projection exceptions only.** The only allowed “intelligence” at projection boundaries: [`ConversationItemContent::is_meaningful`](../crates/baml-rt-conversation/src/view.rs) and discarding `ToolOutcome::StatusOnly` (see [`provenance_item_to_projection_item`](../crates/baml-rt-conversation/src/projection.rs)). `StatusOnly` is stripped **before** `PromptProjectionItem` in the a2a path, per [`prompt_projection`](../crates/baml-rt-tools/src/prompt_projection.rs) module docs. Do not add read-time graph deduplication in this crate. |
| **R4** | **Citations and ref prefixes** for episode replay: wire and render paths live in [`render`](../crates/baml-rt-conversation/src/render.rs) (`prefix_wire_citation`, `render_episode`); semantic contract is in [citable-history-and-checked-citations.md](citable-history-and-checked-citations.md). |
| **R5** | **`assemble_session_history` contract** ([`session_history.rs`](../crates/baml-rt-conversation/src/session_history.rs)). Takes a pre-built `RefTable` and an `ArchiveReader`-compatible closure; **no** internal store open or I/O. |
| **R6** | **Replay and projection alignment** for `SearchRead` / `PageRead` bodies exposed through the provenance pipeline ([`hydrate_session_step_read_replays`](../crates/baml-rt-provenance/src/surreal_store/conversation_context_pipeline.rs)): use the same paging helpers and `PageLimit` semantics as `format_session_read_body` / `prompt_projection`, not a second hidden line cap. |

### Non-goals (this crate)

- **Token-budget or message-count trimming** of the full list (e.g. LLM `max_tokens` windowing).
- **Automatic summarization** of old turns.
- **Vector RAG** over message text inside this crate.
- **Cross-tenant retention / deletion policy** (belongs in storage/ops).
- **Raw LLM or HTTP transport** (belongs in `baml-rt-a2a`, `baml-rt-quickjs`, etc.).

Those may exist elsewhere in the OS; they are not required or implemented here.

---

## C. Ecosystem and comparative review

This is not feature-parity marketing; it names how other stacks shape **agent-visible history** so the gap table (section D) is legible. Prefer official product docs for details.

**LangChain / LangGraph (Python, OSS)**
Chat history is a list of messages; [trim_messages](https://python.langchain.com/api_reference/core/messages/langchain_core.messages.utils.trim_messages.html) (and similar) enforces **token** or **turn** windows with strategies such as “last N”, optional system-message preservation, and optional summarization middleware. Contrast: this codebase keeps a **provenance graph** and typed **tool session** FSM, and uses **line/page** policy in `prompt_projection` and `archive_read`, not a generic message trimmer in `baml-rt-conversation`.

**OpenAI Chat Completions / Assistants (hosted APIs)**
Messages are roles + content (often multipart); threads/runs add **session** state. History is a **linear message list** with API-defined limits. Contrast: history rows here include **tool** and **session step** dimensions with archive refs (`@N`) and history refs (`#N`); read-path consistency is graph-backed in provenance.

**Anthropic Messages API**
Structured user/assistant blocks with tool use and optional **prompt caching** breakpoints. “History” is the payload to one call. Contrast: **storage** (provenance), **view projection** (this crate), and **BAML** injection are separate layers; cache breakpoints are a transport concern, not a type in this crate.

**Agent SDKs (e.g. Google ADK, vendor agents)**
Typically combine **session state** (scratchpad) with a **reduced** chat log. Contrast: emphasis on **reproducible** transcript and episode for drift and evaluation ([`episode` module](../crates/baml-rt-conversation/src/episode/mod.rs)), not an opaque state bag.

**Observability (e.g. LangSmith, W&B Traces)**
Traces are **execution** telemetry (spans, tools, latency). Contrast: **provenance** and **citable history** are **grounding** and audit-oriented; they complement traces but do not replace them.

---

## D. Gap analysis

### Matrix

| Capability | baml-rt-conversation | baml-rt-tools / other crates | Not planned (here) |
|------------|------------------------|------------------------------|--------------------|
| Structured messages + tool calls + session FSM | View types, projection pairs | `prompt_projection`, `ToolRegistry`, `RefTable` | — |
| Token/turn windowing for LLM budget | — | — | Policy would be upstream or a future layer above the graph reader |
| Line/page caps for **display** in prompts | Via `ProjectionRenderOptions` / `PageLimit` | `prompt_projection`, `archive_read` | Arbitrary per-line **hidden** limits not aligned with `PageLimit` (R6) |
| Summarizing old context | — | — | Optional future summarizer is separate from this crate |
| RAG over past messages | — | — | Not here; ref-table `grep`/`read` is **archive**-centric |
| Thread / context ID | Carried on types where relevant | `baml-rt-provenance` store | Multi-channel merge of unrelated threads |
| Citations and drift | Episode DTOs, render | Embedding, effect subscriber, provenance | — |

### Narrative

**Strengths (intentional).** A single **view model** feeds BAML, HTTP, and episode replay; projection policy is centralized in `baml_rt_tools::prompt_projection` and shared options. Graph-backed provenance is the **source of truth** for what appears in history (read-path invariants: [store](../crates/baml-rt-provenance/src/store.rs)).

**Gaps (scoping, not “bugs”).** This crate does not own LLM **token** accounting, **automatic summarization**, or **retrieval** over the message corpus. **Gap in ecosystem terms:** users expecting LangChain-style `trim_messages` implement that **above** the graph or in a future policy module that still outputs `ProvenanceConversationContextItem` rows for this crate to project.

**Future hooks.** A `conversation_policy` (or similar) could sit **above** the graph reader and **slice** `Vec<ProvenanceConversationContextItem>` before projection—without changing this crate’s pure functions.

---

## E. Traceability

| ID | Code / docs | Tests and notes |
|----|-------------|-----------------|
| R1 | [lib.rs scope](../crates/baml-rt-conversation/src/lib.rs); [projection](../crates/baml-rt-conversation/src/projection.rs) | [surreal_snapshot_test](../crates/baml-rt-provenance/tests/surreal_snapshot_test.rs) (among others) |
| R2 | [session_history](../crates/baml-rt-conversation/src/session_history.rs); [episode_session_history_projection_options](../crates/baml-rt-tools/src/prompt_projection.rs) | [episode_reader_integration](../crates/baml-rt-provenance/tests/episode_reader_integration.rs) |
| R3 | [view::is_meaningful](../crates/baml-rt-conversation/src/view.rs); [provenance_item_to_projection_item](../crates/baml-rt-conversation/src/projection.rs) | [prompt_projection tests](../crates/baml-rt-tools/src/prompt_projection.rs) (`#[cfg(test)]`); `session_history_renders_correctly_through_full_pipeline` in [a2a_transport](../crates/baml-rt-a2a/src/a2a_transport.rs) |
| R4 | [render](../crates/baml-rt-conversation/src/render.rs) | Episode reader integration; [citable-history](citable-history-and-checked-citations.md) |
| R5 | [session_history](../crates/baml-rt-conversation/src/session_history.rs) | [EpisodeReader / reader](../crates/baml-rt-provenance/src/episode/reader.rs) + integration tests |
| R6 | [hydrate_session_step_read_replays](../crates/baml-rt-provenance/src/surreal_store/conversation_context_pipeline.rs); [prompt_projection](../crates/baml-rt-tools/src/prompt_projection.rs) | e.g. `read_default_view_after_send_done_should_not_reinline_payload` in [prompt_projection tests](../crates/baml-rt-tools/src/prompt_projection.rs) |

| Integration | Representative test |
|-------------|------------------------|
| a2a end-to-end projection | `session_history_renders_correctly_through_full_pipeline` in [a2a_transport](../crates/baml-rt-a2a/src/a2a_transport.rs) |
| Provenance store → types | [episode_reader_integration](../crates/baml-rt-provenance/tests/episode_reader_integration.rs), [surreal_snapshot_test](../crates/baml-rt-provenance/tests/surreal_snapshot_test.rs) |

---

## F. Cross-links and maintenance

- Short map: [agent-conversation-crate.md](agent-conversation-crate.md)
- Crate `//!` entry: [lib.rs](../crates/baml-rt-conversation/src/lib.rs)
- Developer index: [CLAUDE.md](../CLAUDE.md)

When changing projection rules, replay hydration, or view types, update **sections B and E** in the same PR if behavior or traceability changes.
