# Agentic Memory Plan

## Goal

Design an enterprise-grade memory architecture for this runtime that:

1. Preserves context for multi-agent workflows (coordinator + subagents).
2. Supports direct user-to-agent consultation threads (agent-specific chat memory).
3. Keeps delegated execution deterministic, auditable, and low latency.
4. Works with dynamic agent combinations per run (`clickup+notion` today, `slack+github` tomorrow).

This document focuses on **context for interaction between agents** and related memory behavior.

## Current Baseline In This Repo

1. Conversation history is currently injected into prompts via `ctx.tags.conversation_history`.
2. Context projection already exists in runtime (`TaskStoreConversationContextProvider`).
3. Provenance is persisted in GraphQLite/SQLite (`:memory:` by default, file-backed optional).
4. There is already a `baml-tools-memory` bundle, backed by `agentic-memory` (`~/.brain/{agent}.amem`).
5. Agents like ClickUp/Notion use ReAct loops and rely on projected conversation history to recover IDs/state.

Implication: the system has building blocks, but orchestration-critical state is still too dependent on LLM parsing of transcript history.

## Problem Statement

The architecture must separate three concerns:

1. **Execution context** (IDs, resolved targets, run artifacts, idempotency keys).
2. **Consultation memory** (agent-local user chats, summaries, preferences).
3. **Audit/provenance** (what happened, when, by whom, with which tools).

Today these concerns are partially mixed in conversation history and tool outputs, creating:

1. Token overhead.
2. Risky reuse logic under parallelism.
3. Hidden coupling across subagent calls.
4. Hard cache invalidation when external systems change (new teams/lists/spaces).

## Critical Distinction: Conversation History vs Semantic Memory

These are different systems and must not be conflated:

1. **Conversation history** (short-term, sequence-accurate):
   - Chronological event stream used to reconstruct what happened in a run/thread.
   - Best for replay, deterministic recovery, and recent context projection.
   - Not ideal as the primary retrieval engine for durable knowledge.

2. **Semantic memory** (long-term, retrieval-oriented):
   - Indexed summaries/facts/documents optimized for relevance search.
   - Best for recall across long time ranges and compact context injection.
   - Not ideal as the canonical source for execution-critical IDs unless validated.

3. **Execution context** (typed, deterministic state):
   - Key/value facts required to perform actions (`list_id`, `repo_id`, `channel_id`).
   - Must be explicit, validated, fresh, and versioned.
   - This is neither raw transcript nor semantic retrieval output.

If these three are merged into one blob, systems become slow, brittle, and difficult to parallelize safely.

## Comparison with OpenClaw (What to Adopt, What to Avoid)

`openclaw_memory.md` is useful because it cleanly separates transcript and semantic retrieval layers, but it relies on transcript-mediated cross-agent state recovery.

### Good ideas to adopt

1. **Explicit short-term transcript layer** (append-only event history).
2. **Compaction-aware memory flush bridge** from transcript to durable memory.
3. **Hybrid semantic retrieval** (keyword + vector), not vector-only.
4. **Retrieval quality controls**:
   - MMR/diversity reranking
   - temporal decay
   - keyword fallback when embeddings are unavailable
5. **Backend pluggability + fallback** (primary backend with graceful fallback).
6. **Context visibility policy** for cross-session / cross-agent access scopes.
7. **Embedding provider abstraction + health probing** to avoid hard runtime coupling.

### Things to avoid copying directly

1. **No structured orchestration store** (OpenClaw explicitly lacks one).
2. **State sharing via raw transcript only**.
3. **Implicit cross-agent context transfer without typed capsules**.

Our plan should keep OpenClaw's retrieval strengths while preserving typed orchestration state as source of truth.

## Architecture Decision (Recommended)

Use a **hybrid memory architecture**:

1. **Coordinator-owned structured context store** for orchestration-critical state (source of truth).
2. **Agent-local consultation memory namespaces** for direct chats (optional for delegation).
3. **Existing provenance store** as audit/event trail, linked to memory references.
4. **Redis as cache + locking layer** (optional but recommended at scale).

### Why this is best here

1. Fits the existing coordinator + `system/internal_a2a` orchestration model.
2. Preserves A2A agent independence: subagents can be stateless for delegated work.
3. Still allows each subagent to maintain direct user consultation memory.
4. Avoids requiring LLM transcript parsing for correctness.

## Storage Options: SQLite vs Redis vs In-Memory

### In-memory only (SQLite `:memory:`)

Pros:

1. Fastest local development.
2. No extra infrastructure.
3. Same SQL interface as file-backed mode (zero code changes to switch).

Cons:

1. Not durable across process restarts.
2. Not shareable across distributed runners.
3. Unsuitable for enterprise reliability.

Use:

1. Local tests and demos only.

### Redis only

Pros:

1. Very low latency.
2. Good TTL and lock primitives.

Cons:

1. Weak fit as sole source of truth for complex relational context.
2. Harder long-term audit and ad hoc querying.
3. Durability/consistency tradeoffs without extra setup.

Use:

1. Cache/ephemeral acceleration layer, not sole memory authority.

### SQLite (file-backed)

Pros:

1. Durable source of truth with zero-infrastructure deployment.
2. Already used in this repo for provenance (GraphQLite/SQLite).
3. Supports vector search via `sqlite-vec` extension.
4. Supports full-text search via built-in FTS5.
5. Single `.db` file — trivial backup, migration, and embedding in the runtime binary.
6. WAL mode provides good concurrent read performance.

Cons:

1. Single-writer constraint limits write-heavy concurrent workloads.
2. No built-in distributed access (single-node only without external sync).
3. Less ad-hoc query tooling than Postgres.

Use:

1. Primary persistent store for memory/context metadata, vector index, and FTS index.

### Recommended setup

1. **SQLite (file-backed, WAL mode)** as canonical memory/context store + vector index + FTS index.
2. **Redis** for short-lived caches, idempotency windows, and distributed locks (when needed at scale).
3. **SQLite `:memory:`** fallback for local/dev test mode.

## Should this be a Tool, Agent, or Both?

### Recommended: host **tool bundle** first-class (`system/agentic_memory/*`)

Reason:

1. Context operations are deterministic infrastructure operations.
2. They should not require another LLM agent hop.
3. Coordinator and subagents need fast typed APIs for read/write/checkpoint operations.

### Optional: `memory-agent` facade later

Reason:

1. Useful for human-facing introspection ("what do we remember?").
2. Useful for admin workflows.

But:

1. Do not use a memory agent as the critical execution path for orchestration state.

## Memory Layers

### Layer 0: Conversation History (append-only event stream)

Purpose:

1. Preserve accurate, chronological session/run events.
2. Feed deterministic context projection (`ctx.tags.conversation_history`-like inputs).
3. Support replay/debug and fallback recovery.

Notes:

1. This layer should be compacted/projected for prompts, not injected raw by default.
2. It is not the primary place to "remember" execution IDs long-term.

### Layer 1: Orchestration Context Store (Coordinator source of truth)

Purpose:

1. Run-scoped execution state and resolved IDs.
2. Deterministic delegation inputs.
3. Retry/resume/idempotency behavior.

Examples:

1. `clickup.list_id=901325431486`
2. `clickup.team_id=9013491519`
3. `notion.page_ids=[...]`
4. `idempotency_key=<tenant:run:item>`

### Layer 2: Consultation Memory Store (agent direct-chat memory)

Purpose:

1. Preserve direct user-agent discussions.
2. Support "continue where we left off" in direct consultation mode.

Examples:

1. User preference notes.
2. Previously discussed channels/repos.
3. Agent-specific context capsules/summaries.

### Layer 3: Semantic Memory / Retrieval Index

Purpose:

1. Durable recall across long horizons.
2. Searchable knowledge for context enrichment (not direct write execution).
3. Compact retrieval snippets for prompt injection.

Design notes:

1. Keep source tags (`memory`, `sessions`, `documents`, etc.) to control trust and ranking.
2. Hybrid retrieval (BM25 + vector) should be first-class.
3. Retrieval results should carry citations and freshness metadata.

### Layer 4: Provenance (immutable audit trail)

Purpose:

1. Traceability and postmortem.
2. Replay/diagnostics.
3. Regulatory audit.

## Namespacing Model

All memory keys/records must include:

1. `tenant_id`
2. `user_id` (or service principal)
3. `agent_package`
4. `agent_instance`
5. `thread_id` (for consultation)
6. `run_id/context_id/task_id` (for orchestration)
7. `scope` (`orchestration`, `consultation`, `shared`)

This prevents accidental cross-tenant, cross-user, and cross-agent leakage.

## Data Model (SQLite Canonical)

### Table: `agent_threads`

Purpose:

1. Track direct consultation threads and ownership.

Fields:

1. `thread_id` (pk)
2. `tenant_id`
3. `user_id`
4. `agent_package`
5. `agent_instance`
6. `created_at`
7. `updated_at`
8. `status`

### Table: `session_transcript_events`

Purpose:

1. Store append-only normalized transcript events for deterministic replay and context projection.

Fields:

1. `event_id` (pk, monotonic)
2. `tenant_id`
3. `thread_id`
4. `run_id` (nullable)
5. `context_id` (nullable)
6. `task_id` (nullable)
7. `role` (`user`, `assistant`, `tool_call`, `tool_result`, `system`)
8. `content_json`
9. `token_estimate`
10. `created_at`

### Table: `memory_context_records`

Purpose:

1. Store typed key/value context records with freshness metadata.

Fields:

1. `record_id` (pk)
2. `tenant_id`
3. `scope_type` (`run`, `thread`, `shared`)
4. `scope_id` (e.g., `run_id`, `thread_id`)
5. `namespace` (e.g., `clickup`, `notion`, `slack`)
6. `key` (e.g., `list_id`, `team_id`)
7. `value_json`
8. `version`
9. `resolved_at`
10. `expires_at` (nullable)
11. `source` (`tool_result`, `user_input`, `imported`, `system`)
12. `confidence`
13. `created_at`
14. `updated_at`

### Table: `semantic_documents`

Purpose:

1. Track semantic-memory source documents and indexing metadata.

Fields:

1. `document_id` (pk)
2. `tenant_id`
3. `agent_package`
4. `source_type` (`memory`, `session_summary`, `external_doc`, `imported`)
5. `source_ref` (path/URI/logical ref)
6. `content_hash`
7. `mtime`
8. `created_at`
9. `updated_at`

### Table: `semantic_chunks`

Purpose:

1. Store chunked retrieval units and metadata (non-vector fields).

Fields:

1. `chunk_id` (pk, TEXT)
2. `document_id` (FK to `semantic_documents`)
3. `tenant_id`
4. `chunk_text`
5. `start_offset`
6. `end_offset`
7. `created_at`

### Virtual table: `semantic_chunks_vec` (sqlite-vec)

Purpose:

1. Store embedding vectors for KNN retrieval, co-located with chunk metadata in the same SQLite database.

Definition:

```sql
CREATE VIRTUAL TABLE semantic_chunks_vec USING vec0(
    chunk_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);
```

Joined to `semantic_chunks` by `chunk_id`.

### Virtual table: `semantic_chunks_fts` (FTS5)

Purpose:

1. Full-text keyword index for BM25 scoring, used alongside vector search for hybrid retrieval.

Definition:

```sql
CREATE VIRTUAL TABLE semantic_chunks_fts USING fts5(
    chunk_id UNINDEXED,
    chunk_text,
    content='semantic_chunks',
    content_rowid='rowid'
);
```

Synced from `semantic_chunks` via triggers on INSERT/UPDATE/DELETE.

### Table: `memory_links`

Purpose:

1. Map related context records (lineage and derivation).

Fields:

1. `link_id`
2. `from_record_id`
3. `to_record_id`
4. `relation_type` (`derived_from`, `supersedes`, `copied_from`, `validated_by`)
5. `created_at`

### Table: `delegation_context_capsules`

Purpose:

1. Store compact snapshots passed from coordinator to subagent calls.

Fields:

1. `capsule_id`
2. `tenant_id`
3. `run_id`
4. `target_agent_package`
5. `target_agent_instance`
6. `capsule_json`
7. `created_at`
8. `expires_at`

### Table: `idempotency_records`

Purpose:

1. Enforce safe retries and duplicate prevention.

Fields:

1. `idempotency_key` (pk)
2. `tenant_id`
3. `operation`
4. `status` (`in_progress`, `succeeded`, `failed`)
5. `result_ref`
6. `created_at`
7. `updated_at`
8. `ttl_expires_at`

## Concrete Libraries

### Embedding computation: `fastembed-rs`

In-process embedding model. No API calls, no network dependency, no billing.

1. Crate: `fastembed` (Rust, loads ONNX models).
2. Default model: `BAAI/bge-small-en-v1.5` — 384-dimensional vectors, ~30MB model file.
3. Latency: sub-millisecond for short texts on CPU.
4. Model file ships alongside the binary or is downloaded on first use to a cache directory.
5. No GPU required.

Fallback: if higher-quality embeddings are needed later, swap in an API-based provider (OpenAI `text-embedding-3-small`, Cohere `embed-v4`) behind the `EmbeddingProvider` trait without changing storage or retrieval code.

### Vector storage: `sqlite-vec`

1. SQLite extension — loaded via `rusqlite`'s `load_extension()` or compiled statically.
2. Provides `vec0` virtual table type for KNN search.
3. Vectors stored on-disk inside the same `.db` file as all other tables.
4. Supports exact and approximate nearest neighbor search.
5. Query operator: `WHERE embedding MATCH :query_vec` with `ORDER BY distance LIMIT k`.

### Full-text search: SQLite FTS5 (built-in)

1. No extension needed — FTS5 is compiled into most SQLite builds including `rusqlite` defaults.
2. Provides BM25 ranking via `rank` column.
3. Content-sync mode keeps FTS index automatically in sync with source table via triggers.

### Text chunking: `text-splitter`

1. Crate: `text-splitter` (Rust, semantic-aware chunking).
2. Supports token-count-based splitting with tiktoken tokenizers.
3. Configurable chunk size and overlap.

### SQLite driver: `rusqlite`

1. Already compatible with this repo's SQLite usage patterns.
2. Supports `load_extension()` for `sqlite-vec`.
3. Supports `bundled` feature for self-contained builds.

## Rust Trait Contracts

### `EmbeddingProvider`

Abstraction over embedding computation. Allows swapping local (fastembed) and remote (API) providers.

```rust
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed one or more text chunks into dense vectors.
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// Embedding dimensionality (e.g. 384 for bge-small-en-v1.5).
    fn dimension(&self) -> usize;

    /// Health probe for fallback decisions.
    async fn health(&self) -> ProviderHealth;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHealth {
    Healthy,
    Degraded,
    Down,
}
```

Concrete implementations:

1. `FastEmbedProvider` — wraps `fastembed::TextEmbedding`. In-process, always `Healthy` unless model load fails.
2. `ApiEmbeddingProvider` (future) — wraps HTTP client to OpenAI/Cohere/Voyage. Health probe via latency + error rate.

### `SemanticIndex`

Abstraction over vector storage + hybrid retrieval. Decouples retrieval logic from SQLite specifics.

```rust
#[async_trait]
pub trait SemanticIndex: Send + Sync {
    /// Index a batch of chunks with their embeddings.
    async fn index_chunks(&self, chunks: &[ChunkRecord]) -> Result<(), SemanticIndexError>;

    /// Remove chunks by IDs (cascade from document deletion).
    async fn remove_chunks(&self, chunk_ids: &[&str]) -> Result<(), SemanticIndexError>;

    /// Hybrid search: vector KNN + FTS5 BM25 merged by configurable weights.
    async fn search(&self, query: &SemanticQuery) -> Result<Vec<SemanticHit>, SemanticIndexError>;
}

pub struct ChunkRecord {
    pub chunk_id: String,
    pub document_id: String,
    pub tenant_id: String,
    pub chunk_text: String,
    pub embedding: Vec<f32>,
    pub start_offset: usize,
    pub end_offset: usize,
}

pub struct SemanticQuery {
    pub query_text: String,
    pub query_embedding: Vec<f32>,
    pub tenant_id: String,
    pub namespace_filter: Option<Vec<String>>,
    pub source_type_filter: Option<Vec<String>>,
    pub vector_weight: f32,   // default 0.7
    pub keyword_weight: f32,  // default 0.3
    pub limit: usize,         // default 10
    pub temporal_decay: Option<TemporalDecayConfig>,
    pub mmr_lambda: Option<f32>,  // None = no MMR reranking
}

pub struct SemanticHit {
    pub chunk_id: String,
    pub document_id: String,
    pub chunk_text: String,
    pub source_ref: String,
    pub vector_score: f32,
    pub keyword_score: f32,
    pub final_score: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
```

Concrete implementation: `SqliteSemanticIndex` — wraps a `rusqlite::Connection` and executes the hybrid queries below.

## Reference Queries (SQLite)

### Vector-only KNN search

```sql
SELECT v.chunk_id, v.distance
FROM semantic_chunks_vec v
JOIN semantic_chunks c ON c.chunk_id = v.chunk_id
JOIN semantic_documents d ON d.document_id = c.document_id
WHERE v.embedding MATCH :query_embedding
  AND d.tenant_id = :tenant_id
ORDER BY v.distance
LIMIT :k;
```

### FTS5 keyword-only search

```sql
SELECT f.chunk_id, f.rank AS bm25_score
FROM semantic_chunks_fts f
JOIN semantic_chunks c ON c.chunk_id = f.chunk_id
JOIN semantic_documents d ON d.document_id = c.document_id
WHERE semantic_chunks_fts MATCH :keyword_query
  AND d.tenant_id = :tenant_id
ORDER BY f.rank
LIMIT :k;
```

### Hybrid search (vector + BM25 merge)

Two-phase approach (both queries run, results merged in Rust):

1. Run vector KNN query — get top-N candidates with `distance` (cosine).
2. Run FTS5 query on same `keyword_query` — get top-N candidates with `rank` (BM25).
3. Merge in Rust using:

```
final_score = (vector_weight * (1.0 - distance)) + (keyword_weight * normalize(bm25_score))
```

4. Re-sort merged set by `final_score`, deduplicate by `chunk_id`, apply MMR if configured.

Rationale: SQLite does not support cross-virtual-table joins in a single query (`vec0` and `fts5` are separate virtual tables). The two-phase merge in application code is the standard pattern and gives full control over weight tuning, MMR, and temporal decay without fighting SQLite's query planner.

### FTS5 sync triggers (keep FTS in sync with source table)

```sql
CREATE TRIGGER semantic_chunks_ai AFTER INSERT ON semantic_chunks BEGIN
    INSERT INTO semantic_chunks_fts(rowid, chunk_text)
    VALUES (new.rowid, new.chunk_text);
END;

CREATE TRIGGER semantic_chunks_ad AFTER DELETE ON semantic_chunks BEGIN
    INSERT INTO semantic_chunks_fts(semantic_chunks_fts, rowid, chunk_text)
    VALUES ('delete', old.rowid, old.chunk_text);
END;

CREATE TRIGGER semantic_chunks_au AFTER UPDATE ON semantic_chunks BEGIN
    INSERT INTO semantic_chunks_fts(semantic_chunks_fts, rowid, chunk_text)
    VALUES ('delete', old.rowid, old.chunk_text);
    INSERT INTO semantic_chunks_fts(rowid, chunk_text)
    VALUES (new.rowid, new.chunk_text);
END;
```

## Redis Responsibilities

Use Redis for:

1. Distributed lock per mutable context key (`lock:tenant:scope:namespace:key`).
2. Hot cache for recent context records.
3. Short-lived idempotency window checks.
4. Coordination primitives for high-concurrency fanout.
5. Embedding/query result cache for hot semantic queries.

Do not use Redis as sole durable memory authority.

## API / Tool Surface (Proposed)

Add a new system tool bundle, for example:

1. `system/agentic_memory_put`
2. `system/agentic_memory_get`
3. `system/agentic_memory_resolve`
4. `system/agentic_memory_project`
5. `system/agentic_memory_link`
6. `system/agentic_memory_invalidate`
7. `system/agentic_memory_checkpoint`
8. `system/agentic_memory_search` (hybrid semantic retrieval)
9. `system/agentic_memory_read` (read source slice/citation by reference)
10. `system/agentic_memory_flush` (pre-compaction memory flush trigger)

Each follows existing session FSM (`Open -> Send -> Next -> Finish`).

### `agentic_memory_resolve` behavior

Input:

1. scope + namespace + desired key set + freshness policy.

Output:

1. best record(s), freshness metadata, validity status.

If stale/invalid:

1. returns `needs_refresh=true` with policy hints.

### `agentic_memory_project` behavior

Purpose:

1. Provide compact, token-bounded context capsule for prompt injection.

Supports:

1. role budgets (`message`, `tool_result`, `state`).
2. token cap target.
3. deterministic ordering and dedupe.

### `agentic_memory_search` behavior

Purpose:

1. Hybrid semantic retrieval across approved sources.

Supports:

1. query + namespace/source filters
2. vector + keyword weighted merge
3. optional MMR reranking for diversity
4. optional temporal decay weighting
5. fallback to keyword-only if embeddings unavailable

Output:

1. ranked hits with `source_ref`, citation range, score breakdown, and freshness.

## Compaction and Memory Flush Bridge

Adopt a controlled bridge similar to OpenClaw's pre-compaction flush, but with typed outputs:

1. When transcript/token budget reaches threshold:
   - trigger `agentic_memory_flush`.
2. Flush produces:
   - run summary capsule
   - extracted durable facts (typed)
   - optional semantic document updates
3. Mark flush checkpoint in run metadata to avoid duplicate flush in same compaction cycle.

Benefits:

1. Keeps prompt context small.
2. Preserves durable intent/facts.
3. Avoids losing critical state during transcript compaction.

## Freshness and Invalidation Policy

All context records should carry:

1. `resolved_at`
2. optional `expires_at`
3. validation status (`unknown`, `valid`, `stale`, `invalid`)

Policy:

1. Reuse when valid and within TTL.
2. On write failure (`not found`, `forbidden`, `archived`, etc.), mark invalid and refresh targeted scope.
3. For user-specified targets (team/list IDs or names), always prioritize explicit user constraint over cached defaults.
4. For semantic hits, require source validation before promoting a value into execution context records.

## Delegation Contract Changes (Important)

Current delegated calls mainly pass prompt text. Add explicit context payloads:

1. `context_capsule` (typed JSON with resolved IDs and constraints)
2. `context_refs` (record IDs for traceability)
3. existing same-session same-agent conversation context remains available as continuity/fallback

Default for delegated execution:

1. Explicit typed state via `context_capsule`.
2. Existing conversation history projection remains continuity context, not source of truth for execution-critical state.

Delegation fallback order:

1. capsule (typed state)
2. projected same-session same-agent history
3. semantic recall snippets (bounded, cited)

## Context Visibility and Access Policy

Adopt explicit visibility scopes inspired by OpenClaw session visibility:

1. `self` — only same thread/session.
2. `run` — only records in same orchestration run.
3. `agent` — same agent package/instance.
4. `tree` — parent/child spawned runs only.
5. `tenant` — all tenant-scoped records (policy-gated).

Every memory tool call must declare requested scope; policy layer enforces least privilege.

## Retrieval Quality Controls

To strengthen semantic recall quality:

1. Hybrid scoring:
   - `final_score = (vector_weight * vector_score) + (keyword_weight * keyword_score)`
2. MMR reranking (optional) for diversity.
3. Temporal decay (optional) for time-sensitive sources.
4. Evergreen sources (policy docs, canonical configs) bypass decay.
5. Query expansion for keyword-only fallback (stop-word removal, locale-aware tokenization).
6. Source trust weighting:
   - orchestration checkpoints > validated tool results > user notes > raw transcript snippets.

## Provider/Backend Health and Fallback

Add explicit health probes and fallback behavior:

1. embedding provider probe (`healthy`, `degraded`, `down`)
2. vector index probe
3. fallback modes:
   - hybrid -> keyword-only
   - primary backend -> secondary backend
4. expose provider/fallback status in memory tool responses and metrics.

## Interaction Patterns

### Pattern A: Coordinator -> ClickUp create task (delegated, deterministic)

1. Coordinator resolves `list_id` via memory tool.
2. If stale/missing, coordinator runs discovery and updates memory.
3. Coordinator delegates create call with explicit `list_id` in capsule.
4. Subagent executes write; no transcript parsing required for correctness.
5. Result and updated state checkpointed to memory + provenance link.

### Pattern B: User direct consults ClickUp agent

1. Thread-specific consultation memory is loaded for that agent thread.
2. Agent can use that memory for user experience.
3. When coordinator later delegates to ClickUp agent, memory is not implicitly imported unless requested by policy.

### Pattern C: User requests "use what I discussed with Slack earlier"

1. Coordinator calls memory tool to import specific Slack thread summary/capsule.
2. Imported records are linked (`copied_from`) and versioned in run scope.
3. Delegation happens with explicit imported capsule.

## Security and Compliance

1. Strict tenant/user scoping on every memory query.
2. At-rest encryption for SQLite databases and Redis channels.
3. Sensitive field encryption/tokenization in `value_json`.
4. PII tagging and retention policies per namespace.
5. Full audit trail links from context records to provenance events.

## Reliability and Concurrency

1. Optimistic concurrency with `version` checks on record updates.
2. Distributed locks for high-contention keys.
3. Idempotency keys for external writes.
4. Exactly-once semantics where possible, at-least-once with de-dup where not.
5. Deterministic retries with bounded backoff and explicit failure reasons.

## Observability

Metrics:

1. memory hit/miss ratio
2. stale/invalid rate
3. refresh latency
4. capsule size (tokens/bytes)
5. delegation failure due to missing context

Logs:

1. include `tenant_id`, `run_id`, `context_id`, `agent_package`, `namespace`, `key`.

Tracing:

1. add spans around memory resolve/project/invalidate and link to existing A2A spans.

## Backward Compatibility Strategy

1. Keep `ctx.tags.conversation_history` behavior unchanged initially.
2. Introduce memory capsules as additive input.
3. Move critical flows (ClickUp list/team/space reuse) to capsule-first.
4. Gradually reduce transcript dependence for deterministic operations.

## Rollout Plan

### Phase 0: Design + contracts

1. Define schema and tool contracts.
2. Define context capsule JSON schema.
3. Define freshness and invalidation policy matrix per namespace.

### Phase 1: In-process MVP (no new infra)

1. Implement `system/agentic_memory_*` with in-memory backend.
2. Wire coordinator to use resolve/checkpoint for ClickUp IDs.
3. Add capsule injection to delegated prompts.
4. Keep transcript fallback enabled.
5. Implement Layer-0 transcript projection budgets + policy filtering.

### Phase 2: SQLite file-backed durable backend

1. Switch from `:memory:` to file-backed SQLite (WAL mode).
2. Add migrations and indexing for all memory tables.
3. Add namespace-based retention jobs.
4. Add `session_transcript_events` + typed context tables.

### Phase 3: Semantic retrieval (fastembed-rs + sqlite-vec + FTS5)

1. Integrate `fastembed-rs` (`FastEmbedProvider` behind `EmbeddingProvider` trait).
2. Add `sqlite-vec` extension loading and `semantic_chunks_vec` virtual table.
3. Add FTS5 `semantic_chunks_fts` virtual table with sync triggers.
4. Implement `SqliteSemanticIndex` behind `SemanticIndex` trait.
5. Implement `agentic_memory_search` hybrid retrieval (two-phase vector + BM25 merge).
6. Add MMR/temporal decay/query-expansion controls.
7. Add compaction-triggered memory flush/checkpoint flow.

### Phase 4: Redis acceleration (when scale requires it)

1. Add hot-key cache.
2. Add distributed locks for refresh/write-critical keys.
3. Add cache invalidation hooks from update paths.
4. Add embedding/query result cache for hot semantic queries and idempotency assist.

### Phase 5: Cross-agent consultation import controls

1. Implement explicit import policies.
2. Add user-level consent and policy checks.
3. Add admin introspection endpoints.

## Suggested Repository Touchpoints

1. `crates/tools/system/`  
   Add new system memory tool bundle + metadata/types.

2. `crates/baml-agent-runner/src/main.rs`  
   Register memory bundle behind feature flag and manifest declaration.

3. `crates/baml-rt-a2a/src/a2a_transport.rs`  
   Extend context provider to support capsule projection pipeline.

4. `agents/coordinator-agent/src/index.ts`  
   Use memory resolve/checkpoint around delegation and foreach expansion.

5. `agents/*/baml_src/*_prompt.baml`  
   Add optional typed capsule section and reduce dependence on raw history.

## Recommended Initial Decision (for this codebase now)

1. Implement **new system tool bundle** for orchestration memory first.
2. Use **SQLite file-backed (WAL mode)** as canonical backend, ship `:memory:` MVP first.
3. Use **`fastembed-rs`** (`BAAI/bge-small-en-v1.5`, 384-dim) for in-process embedding — no API dependency.
4. Use **`sqlite-vec`** for vector KNN storage and **FTS5** for BM25 — all in the same `.db` file.
5. Add Redis only when fanout/concurrency requires lock/cache scale.
6. Keep existing `baml-tools-memory` as consultation/semantic memory; do not overload it as orchestration source of truth.

## Risks and Mitigations

Risk: two memory systems (`baml-tools-memory` + orchestration memory) confuse ownership.  
Mitigation: document strict ownership boundaries and enforce via namespaces + APIs.

Risk: stale IDs cause failed writes.  
Mitigation: optimistic write + targeted refresh + retry-once policy.

Risk: token growth from dual context inputs.  
Mitigation: capsule-first and strict token budgets for transcript fallback.

Risk: migration complexity.  
Mitigation: phased rollout with compatibility mode and feature flags.

Risk: semantic memory returns stale/incorrect facts for execution writes.  
Mitigation: semantic recall can suggest, but execution context promotion requires validation + freshness check.

## Open Questions

1. Should orchestration memory be embedded in existing provenance DB first, or separate service from day one?
2. What is acceptable staleness window per integration (`clickup`, `slack`, `github`, etc.)?
3. Which fields require encryption-at-field-level in `value_json`?
4. Should capsule generation happen in coordinator only, or centrally in runtime context provider?
5. Which semantic sources are allowed for execution promotion by policy?
6. Do we need per-integration trust policies (`clickup` vs `github` vs `slack`) for semantic recalls?

## Success Criteria

1. Delegated write operations no longer depend on transcript parsing for required IDs.
2. Parallel fanout reliability improves without duplicate writes.
3. Token usage per delegated call decreases measurably.
4. Cross-agent composition remains plug-and-play with explicit contracts.
5. Direct consultation threads remain high quality without contaminating orchestration correctness.
6. Semantic retrieval quality is measurable (precision/recall or acceptance proxy), with graceful fallback when vectors are unavailable.

---

## Log-Driven Amendments (from `coordinator_logs`)

The baseline plan is directionally correct. The run log highlights additional constraints that should be explicit in implementation.

### Observed runtime pain points

1. **Prompt token growth across repeated delegated turns**:
   - `ChooseClickUpAction` input tokens trend from ~3.5k to ~4.6k during one run.
   - This indicates context accumulation inside `conversation_history` is driving repeated cost/latency.

2. **Infrastructure/tool metadata polluting action prompts**:
   - `system/discover_agents` payloads and coordinator status text are repeatedly injected into subagent history.
   - This creates low-signal context and encourages brittle transcript parsing.

3. **Duplicate external writes in fanout**:
   - Repeated `create_task` calls for semantically duplicated items appear in one orchestration run.
   - Existing prompt rules are not sufficient as a safety boundary without deterministic idempotency.

4. **Single shared context stream for many delegated children**:
   - Multiple child delegations read from the same growing context stream.
   - This increases cross-item contamination risk and harms per-item determinism.

### Architecture upgrades to add to plan

1. **Projection policy engine for conversation history (Layer 0)**
   - Exclude low-value tool results by default from delegated prompts:
     - `system/discover_agents`
     - `system/discover_tools`
     - status-only updates without actionable payload.
   - Enforce token budgets by channel:
     - `history_budget_tokens`
     - `tool_budget_tokens`
     - `message_budget_tokens`
   - Deterministic truncation order: newest-first inside source buckets, then stable merge.

2. **Delegated context views**
   - Extend context projection input with:
     - `run_id`
     - `target_agent_package`
   - Default projection for delegated calls should prioritize:
     1) same-session same-agent operational continuity
     2) run-level capsules
     3) bounded global user-intent fallback carried in the shared `context_id` stream.
   - Do **not** filter transcript history strictly by `task_id`; delegated work may need
     facts discovered in earlier sibling tasks within the same shared run context.

3. **Operation journal + idempotency ledger (execution guardrail)**
   - Before external writes (create/update/delete), coordinator computes an operation key:
     - `tenant + run + target_agent + operation + normalized_payload_fingerprint`
   - Persist with status `in_progress|succeeded|failed`.
   - If key exists in `succeeded`, skip duplicate write and synthesize from prior result.
   - This must gate writes even when model output repeats the same plan.

4. **Foreach normalization hardening**
   - Always dedupe source iterable items before expansion (not only on model-normalized path).
   - Compute stable dedupe keys from canonical fields (`id`, `url`, normalized title, source ref).
   - Persist expanded-child lineage (`foreach_node_id`, `item_key`) for replay and duplicate prevention.

5. **Memory promotion pipeline (history -> typed facts -> semantic docs)**
   - Add explicit promotion stages:
     1) `extract`: candidate facts from tool_result/message
     2) `validate`: schema + freshness + source trust checks
     3) `promote`: write to `memory_context_records`
     4) `index`: optional semantic chunk write
   - Only promoted/validated facts may become capsule inputs for execution-critical writes.

6. **Semantic memory trust gating**
   - Keep semantic retrieval advisory by default.
   - Require `promotion_policy` to move semantic hit -> execution context:
     - must include citation
     - source type allowlisted
     - freshness window satisfied
     - optional live revalidation via tool call for write-critical IDs.

### Implementation order adjustments

Update rollout priorities to reduce duplicate writes early:

1. **Phase 1a (new)**: projection policy + delegated history budgeting + same-session same-agent continuity.
2. **Phase 1b (new)**: idempotency ledger for external writes + foreach dedupe before expansion.
3. **Phase 1c**: capsule-first delegation for ClickUp/Notion critical IDs.
4. Keep existing Phase 2+ (Postgres/Redis/semantic retrieval), but include promotion pipeline contracts before wide semantic rollout.

### Additional success criteria (additive)

1. At least 50% reduction in delegated prompt input tokens for repeated subagent turns.
2. Zero duplicate external writes for identical operation keys within a run.
3. `system/discover_agents` payload excluded from delegated action prompts by default.
4. 95th percentile memory resolve latency and hit ratio tracked per namespace.
5. Every execution-critical capsule field is traceable to a promoted context record with provenance link.
