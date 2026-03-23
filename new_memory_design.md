# [Proposal] Layered Memory for Multi-Agent Runtime

## Summary
This proposal defines a provenance-first memory architecture for our multi-agent runtime where many agents share one `context_id`.

V1 focus:
- Provenance is canonical.
- Add a bounded in-memory projection cache for deterministic key retrieval.
- Resolve is read-through: cache miss -> provenance query -> domain extraction -> cache backfill -> return.
- Keep existing `memory/*` as optional per-agent cognitive memory, not shared-context authority.
- Defer `USER.md` / `MEMORY.md` prompt-profile layer.

## Why Change
Current strengths:
- Provenance already stores messages, tool calls/results, and LLM outputs.
- Shared `context_id` gives a natural cross-agent memory scope.

Current gaps:
- Deterministic key retrieval (e.g. `team_id`) is not standardized for all agents.
- Repeated provenance scans are expensive without a hot-path projection.
- Existing `memory/*` is per-agent `.amem`, not shared context memory.

## Design Goals
1. Keep provenance as source of truth.
2. Provide deterministic, low-latency key retrieval for agents.
3. Standardize a reusable extractor interface for tool developers.
4. Avoid raw-data duplication outside provenance.
5. Keep v1 simple: in-memory store, no DB migrations.

## Non-Goals (v1)
- Prompt-profile memory injection (`USER.md` / `MEMORY.md`).
- Semantic/vector memory as primary path.
- Replacing provenance storage.

## Proposed Architecture

### Layer 0: Provenance (Canonical, Slow Path)
- All runtime events remain in provenance.
- Retrieval supports scoping by `context_id` with optional `agent_id`, `tool_name`, outcome filters, and optional payload text filters.
- Provenance remains the authority for evidence and lineage.

### Layer 1: Projection Cache (Fast Path)
- In-memory map keyed by `(scope, namespace, key)`.
- Scope is `context_id` plus optional `agent_id` overlay.
- Bounded cardinality per `(scope, namespace)` with cap `K` (chosen for v1).

Resolve flow:
1. Check projection cache.
2. On miss, query provenance.
3. Run domain extractor.
4. Write synthesized projection(s) with `source_event_ids` in a batch when available.
5. Return to caller.

## Ownership and Scope

### Ownership
Memory is owned by a shared context memory service (not coordinator-owned state).

### Scopes
1. `agent_context` (agent-specific overlay)
2. `context` (shared for all agents in same `context_id`)
3. `user` (user-authored only in current plan)

Read precedence:
1. `agent_context`
2. `context`
3. `user`

V1 note:
- `user` scope read path may be effectively no-op/empty until user-profile storage is enabled.

Write policy:
- Agents do not write memory entries directly.
- Runtime-only checkpointing writes synthesized projections derived from provenance events/results.
- All writes remain within the same `context_id` scope (with optional `agent_id` overlay).
- `user` scope is user-authored only for now.
- No raw provenance fact copying; only synthesized projections with evidence refs.

## Relationship to Existing `memory/*` Tool Bundle
Current `memory/*` tools are still relevant, but for a different layer:
- Purpose: per-agent cognitive/procedural memory (`~/.brain/{agent}.amem`).
- Not the canonical shared memory for cross-agent deterministic retrieval.

Decision:
- Keep `memory/*` as optional agent-local memory.
- Standardize shared memory through provenance-backed system tools.

## Reusable Extractor Interface (Tool Developer Contract)
Deterministic memory should be pluggable and reusable across tool domains.

Runtime provides:
- extractor trait/interface
- extractor registry
- checkpoint/resolve mechanism
- scope/retention/safety/evidence policies

Tool domains provide:
- extraction logic from domain payloads
- namespace and key conventions
- staleness/update semantics for domain-specific keys

Conceptual extractor contract:
- `tool_pattern()` -> which tool outputs/events it handles
- `namespace()` -> e.g. `clickup`, `notion`, `slack`
- `extract(event_or_result)` -> `Vec<ProjectionEntry>`
- optional hooks: confidence, overwrite policy
- optional hooks: candidate merge policy and active-pointer selection

`ProjectionEntry` requirements:
- synthesized value (not raw payload copy)
- `source_event_ids` attached
- scope + namespace + key

Extractor guidance:
- Prefer extracting a related domain batch in one pass (not only one requested key).
- For singleton keys, latest evidence can overwrite existing cache entry.
- For multi-entity domains (e.g., ClickUp teams/spaces/lists), keep candidate mappings and separate active pointers instead of forcing single-key latest-wins everywhere.
- Runtime should accept multi-entry extractor output and support atomic-like batch backfill into cache for one resolve miss path.

Registration and exposure model:
- Extractors are runtime-internal plugins, not separate agent-facing tools.
- Registry split is explicit: one agent-facing tool registry (`memory/*`, `support/*`, etc.) plus one internal extractor registry consumed by `memory/context_memory_extract`.
- Tool crates can host extractor modules (example: `crates/tools/clickup/src/memory.rs`) and register them by tool pattern/namespace.
- Agents should declare and call only generic memory APIs (`memory/context_memory_resolve`, `memory/context_memory_extract`), not domain-specific memory tool names.
- Domain tools (e.g. `support/clickup`) remain unchanged and independent from the generic memory API surface.

## Summarization Policy
Deterministic first:
- Programmatic summarization from structured JSON outputs.
- Keep stable high-signal fields only.
- Normalize into compact typed entries.

LLM summarization:
- fallback only for unstructured/ambiguous content.

## APIs / Tools (v1)
1. `memory/context_memory_resolve`
- Shared generic context retrieval API over provenance.
- Input: `context_id`, optional `agent_id`, and match/filter parameters.
- Output: raw/compact provenance-derived rows (no deterministic domain extraction contract).

2. `memory/context_memory_extract`
- Deterministic extractor-backed projection API.
- Input: scope + namespace + requested key(s).
- Behavior: projection cache hit -> return; miss -> provenance query -> extractor -> batch backfill -> return.
- Fallback contract when extractor is missing:
  - return explicit fallback metadata (`mode: fallback_raw`, `extractor_found: false`)
  - return raw/compact provenance rows
  - do not claim deterministic field availability

3. `system/provenance_recall`
- Slow-path episodic recall/search for historical questions (separate from deterministic key resolve).
- Intended for queries like: "did we discuss X?", "what happened previously?", "why was Y chosen?".
- Input: `context_id` + query text, with optional filters (`agent_id`, `tool_name`, outcome, time window, `top_k`).
- Output shape:
  ```json
  {
    "summary": "string",          // concise answer or "no relevant evidence found"
    "evidence": [
      {
        "event_id": "string",
        "timestamp_ms": 1234567890,
        "source": "tool_result | message | llm_response",
        "snippet": "string"       // relevant excerpt from the event
      }
    ],
    "detail_rows": []             // optional, full event rows if requested
  }
  ```
- Contract: claims in summaries should be evidence-backed; if no evidence is found, return explicit miss.

Note:
- `memory/context_memory_resolve` is generic context retrieval.
- `memory/context_memory_extract` is deterministic key retrieval (`team_id`, `list_id`, etc.) via domain extractors.
- `system/provenance_recall` is exploratory/historical recall, not key-value resolve.
- `system/memory_flush_before_compress` is deferred (no pre-compaction memory harvesting in v1).
- Prompt-profile APIs (`USER.md` / `MEMORY.md`) are deferred from v1.

## Retention and Lifecycle (Open, with v1 choice)
Candidate policies:
1. Upsert-by-key
2. Event-window TTL
3. Type-based retention
4. Bounded cardinality per `(scope, namespace)`
5. Hybrid

V1 decision:
- Use Option 4 (bounded cardinality), cap `K` per `(scope, namespace)`.
- Add read-through backfill on miss.
- Add short negative-cache for misses.
- Add in-flight dedupe per `(scope, namespace, key)`.

## Observability (Required for v1)
To tune correctness and performance, instrument at least:
- Cache hit/miss ratio per namespace.
- Provenance query latency per extractor/domain.
- Backfill write count (entries written per miss path and total writes).
- Negative-cache effectiveness (repeated misses avoided).

Recommended additions:
- In-flight dedupe collision count (how often duplicate scans were prevented).
- Resolve outcome counters (`hit`, `backfilled_hit`, `hard_miss`, `negative_cache_hit`).

## Retrieval Behavior Details (Agreed)
- Memory starts empty.
- Checkpoint is request-driven (on resolve miss), not eager on every tool completion.
- On miss:
  - query provenance
  - fetch latest relevant matching provenance rows for the domain
  - extract deterministically (prefer batch extraction of related keys)
  - cache synthesized projections in batch
  - return value
- If not found in provenance:
  - store short-lived negative-cache marker
  - return miss quickly on repeated requests until marker expires

Staleness and updates:
- Overwrite for same-key singleton entries is supported (newer evidence wins).
- Multi-candidate domain entries should preserve candidate sets and update active pointers based on newest valid evidence.
- Exact candidate/active-pointer policy is extractor-specific business logic, not a runtime-global rule.

Batch backfill policy:
- Runtime capability: one miss can backfill multiple extracted keys for the same namespace/scope.
- Domain policy (ClickUp v1): on first miss for a ClickUp key, query relevant ClickUp provenance rows for that context, extract related keys (`team_id`, `space_id`, `list_id`, mappings), backfill all, then return requested key.
- This amortizes provenance scan cost across sequential related lookups.

## Data Model (v1 In-Memory)
Projection row shape (conceptual):
- `scope_type` (`context` | `agent_context` | `user`)
- `scope_id` (context id)
- `agent_id` (optional)
- `namespace`
- `key`
- `value_json`
- `projection_type` (`working_set` | `summary` | `preference`)
- `source_event_ids` (required for non-user writes)
- `updated_at`

## Rollout Plan

### Phase 1: Provenance Memory Base Layer

#### 1.1 `memory/context_memory_resolve` Tool Implementation ✅ COMPLETED

**Location**: `crates/tools/memory/src/context_memory_resolve.rs`

**Dependency change**: Add `baml-rt-provenance` to `crates/tools/memory/Cargo.toml` (Option A — memory bundle depends on provenance).

**Types**:

```rust
/// Resource type to query from provenance.
/// Note: `LlmCalls` returns both LLM call invocations and their results
/// as part of the same activity. Similarly, `ToolCalls` returns both
/// tool call invocations and their results.
pub enum ContextMemoryResource {
    LlmCalls,   // includes both call and result payloads
    ToolCalls,  // includes both call and result payloads
    Messages,   // conversation messages
}

/// Outcome filter for queries.
pub enum ContextMemoryOutcome {
    FailedOnly,
    SuccessfulOnly,
    Both,  // default
}

pub struct ContextMemoryResolveSendInput {
    pub resource: ContextMemoryResource,
    pub agent_id: Option<AgentId>,
    pub tool_name: Option<String>,
    pub outcome: Option<ContextMemoryOutcome>,
    pub payload_text: Option<String>,  // FTS5 search pattern
    pub from_timestamp_ms: Option<u64>,
    pub to_timestamp_ms: Option<u64>,
    pub top_k: Option<u32>,
    pub cursor: Option<String>,
}

pub struct ContextMemoryResolveNextOutput {
    pub rows: Vec<ContextMemoryResolveRow>,
    pub total_count: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub done: bool,
}
```

**Implementation notes**:
- Uses existing `ProvenanceOpsQuery` trait from `baml-rt-provenance`.
- No changes needed to provenance crate — existing capabilities are sufficient.
- `payload_text` uses the active provenance backend text-search capability (implementation backend may vary).
- The `tool_name` filter is exact match; pattern search (e.g., "team", "space") uses `payload_text` FTS5.
- `MemoryBundle` gets an optional `Arc<dyn ProvenanceOpsQuery>` for context memory tools.
- The tool is scoped to the calling agent's `context_id` from `ToolSessionContext`.
- Query scope is context-wide across tasks (`task_id` must be `None` for this tool path).

**Text search capability**:
- Provenance query layer provides payload text filtering/search.
- Backend-specific implementation details are intentionally abstracted in this design doc.

**Bundle changes** (`crates/tools/memory/src/bundle.rs`):
- Add `with_provenance(agent_name, query)` constructor.
- Add `context_memory_resolve` handler when provenance is available.

**Wiring requirement**:
- Runner/builder optional bundle registration must use provenance-aware constructor when provenance query service is available.
- Add an integration check that `memory/context_memory_resolve` is discoverable at runtime when `memory/*` is declared.

#### 1.2 Remaining Phase 1 Items (deferred to after 1.1)
- Implement `memory/context_memory_extract` (deterministic extractor-backed retrieval).
- In-memory bounded projection cache.
- Read-through miss path + backfill.
- Negative-cache + in-flight dedupe.

### Phase 2: Reusable Extractor Framework
- Add extractor trait/interface + registry.
- Wire resolver miss path to extractor registry.

### Phase 3: ClickUp Example
- Implement ClickUp extractor in tool domain module (e.g. `tools/clickup/.../memory.rs`).
- Extract keys like `team_id`, `space_id`, `list_id` from provenance tool results.

### Phase 4: Episodic Recall Tool
- Implement `system/provenance_recall` with evidence refs.

### Deferred Phases
- Prompt-profile memory (`USER.md`/`MEMORY.md`).
- Compression-time memory flush.
- Persistent DB-backed projection store + migrations.

## Acceptance Criteria (v1)
1. Any participating agent can use shared generic retrieval and deterministic extraction via standardized memory APIs.
2. On miss, `context_memory_extract` automatically queries provenance, extracts, backfills, and returns when possible.
3. Repeated misses do not trigger repeated expensive scans (negative cache).
4. Repeated hits avoid provenance scans (projection cache works).
5. ClickUp scenario works across turns (e.g., turn 3 resolves `team_id` discovered in turn 1).
6. Provenance remains canonical with evidence linkage for cached projections.

## Test Matrix (Required)
1. Context-wide retrieval:
- `context_memory_resolve` returns data across multiple tasks in same `context_id` (`task_id` filter not applied).

2. Deterministic extraction:
- `context_memory_extract` with registered extractor returns structured deterministic keys.

3. Missing extractor fallback:
- `context_memory_extract` without matching extractor returns `mode: fallback_raw`, `extractor_found: false`, and raw rows.

4. Negative-cache behavior:
- Repeated hard misses avoid repeated provenance scans within negative-cache window.

5. Batch backfill behavior:
- First miss can populate multiple related keys; subsequent related key lookups are cache hits.

6. Wiring validation:
- When agent manifest includes `memory/*` and provenance is configured, `context_memory_resolve` is present in discoverable tool set.

## Open Questions
1. Default `K` value per `(scope, namespace)`.
2. Final deterministic API name: `memory/context_memory_extract` vs `memory/context_memory_project`.
3. Batch vs per-key backfill write behavior (runtime supports both; ClickUp v1 uses batch-on-first-miss).
4. Should `user` scope eventually become tenant+user scoped when enabled.
