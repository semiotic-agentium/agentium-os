# Agentic Memory (Refactor, provenance-first)

## Why this refactor

The previous plan assumed we needed a separate memory substrate as the core path.
On `main`, the runtime has evolved:

- Provenance is now the primary, graph-backed event source.
- Prompt context projection is standardized (`conversation_history` with `#N`/`@N` refs).
- Citations are first-class and validated (`#N`, `@N`, `@N:L`, `!` negation).
- `system/introspection` and `system/extrospection` support scoped querying and archive drilldown.
- Read-drilldown already has bounded budgets (`retrieve_ref` + projection modes + hard caps).

So the right direction is **not** "new memory DB first". It is:

> **Use provenance as memory-of-record, and add a thin retrieval/promotion layer for execution-quality context.**

---

## Main architecture decision

## 1) Memory-of-record = provenance graph

Use existing provenance as the canonical source for:

- user messages
- assistant responses
- tool calls/results
- plan/step citations

No duplicate write-path to a second event store.

## 2) Prompt context ≠ full memory

Do not keep stuffing long conversation history into every prompt.

Use a two-lane model:

- **Lane A: Short recency window** (small projected `conversation_history`)
- **Lane B: On-demand retrieval** via `system/introspection` / `system/extrospection` + `read.retrieve_ref`

## 3) Retrieval remains FSM at runtime (Open -> Send -> Read -> Finish)

Important correction: provenance retrieval is **not** a wire-level one-shot today.

For both `system/introspection` and `system/extrospection`, the runtime contract is:

1. `Open`
2. `Send` (query or `read.retrieve_ref` envelope)
3. `Read` *(often surfaced as "Next" / "continue" in helper APIs and output type names)*
4. `Finish`

This is intentional and should remain the canonical primitive (uniform tooling semantics, streaming compatibility, budgets, provenance telemetry).

### Ergonomic fix (recommended)

Implement coordinator/agent-side helper wrappers that expose one-call ergonomics while preserving FSM internally:

- `queryProvenanceOnce(args)` -> performs Open/Send/Read/Finish
- `retrieveRefOnce(refId, projection)` -> performs Open/Send(read)/Read/Finish

This removes orchestration friction without changing tool contracts.

### Optional facade (sugar only)

A one-shot facade tool can be added later, but must delegate internally to the same FSM path.
Do not make a separate retrieval backend.

## 4) Execution state is promoted, typed, and explicit

For write-critical values (e.g., `team_id`, `list_id`, `space_id`):

- discover from provenance/tool output
- validate freshness
- promote into typed run-state capsule
- delegate with capsule first

This avoids transcript-parsing fragility.

---

## What changed on `main` that we should leverage

1. **Citable history contract is stable**
   - `#N` = projected history entry
   - `@N` = archived output
   - `@N:L` / `@N:L1-L2` = line-scoped evidence

2. **Projection discipline improved**
   - archived payloads are not blindly re-inlined forever
   - read paths use grep/pagination semantics

3. **Provenance tools support drilldown**
   - `read.mode=retrieve_ref`
   - projection levels: `identity | summary | detail`
   - retrieval budgets/caps to prevent runaway context

4. **Provenance ingestion is cleaner**
   - effect-bus-centered recording and stronger consistency for analysis paths

---

## Answering the key question: will LLMs understand `@18`?

**Yes, if the prompt contract is strict and repeated consistently.**

`@18` by itself is opaque semantically, but LLMs do fine with symbolic handles when:

- syntax is simple and stable
- examples are shown repeatedly
- actions are constrained (e.g., "if claim depends on line, cite `@18:42`")
- tooling enforces/validates references

Practical improvement:

- Keep `#/@` as canonical wire format.
- In prompts, also render a short human label next to refs when available:
  - `@18 (clickup/get_tasks result, 500 rows)`
- Keep machine ref unchanged so provenance stays deterministic.

---

## Runtime dataflow after `Send` (what agents actually see)

This is already in place on `main`.

After a tool `Send` completes in step-executor flows (Slack/Notion/ClickUp):

1. The tool result is archived and gets a short ref (for example `@9`).
2. Runtime/provenance records `SendDone` and related session events.
3. Next LLM hop receives updated:
   - `session_context` (FSM state)
   - `ctx.tags['conversation_history']` (projected context rows for the same context)

Note on coordinator-style manual sessions (`openToolSession` + `continue()`):

- coordinator code can inspect `continue()` outputs directly in JS,
- but provenance/history is still updated via the same runtime path.

### `session_context` structure (current)

Current injected shape is intentionally minimal:

```json
{
  "contract_version": "session_context",
  "session_open": true
}
```

It carries FSM state, not full payload bodies.

### What appears in `conversation_history`

`conversation_history` is not reference-only. For session steps it can include:

- archive headers (for example `@9 support/clickup "...summary..."`)
- inline rendered views (`cat -n @9 ...`) when available
- paginated/filtered read views from later `Read` hops

So the model often sees both the ref and bounded content, not just `@9`.

## How `@N` drilldown works today (ClickUp example)

If the model sees `@9 ...` and needs more detail, it emits a `Read` step referencing that archive:

```json
{"op":"Read","input":{"archive_ref":"@9"}}
```

or targeted/paginated:

```json
{"op":"Read","input":{"archive_ref":"@9","grep":"in progress","offset":0,"limit":40}}
```

Then on subsequent hops it can continue paging (`offset += limit`) until enough evidence is gathered.

Important behavior:

- `Read` returns bounded rendered content (cat/grep-style), not unbounded raw dumps.
- The returned read output is appended into projected history for subsequent hops.
- This is exactly how the model gets deeper access to prior `Send` results.

Operationally, for very large outputs, preferred strategy is still:

1. narrow upstream query via a new `Send` when possible
2. use `Read` drilldown only for the slice needed for grounding/citations

## Critical gap observed in logs (current main pain)

The runtime path is correct, but agent behavior is not always aligned:

- prompts show `@N ... cat -n @N ... (more — offset=...)`
- model reasoning recognizes pagination is needed
- but final emitted step sometimes becomes `Finish` instead of `Read`

This is the key blocker for reliable provenance-backed memory usage.

### Why this happens

1. The model under-applies the `CONTINUE` policy despite seeing `more lines` indicators.
2. Step-level completion pressure causes premature `Finish`.
3. The contract "one open session at a time, drive it from Open to Finish before switching" is not enforced strongly enough in prompt discipline.

### Required behavioral contract (explicit)

When a session is open for a tool (e.g., `support/clickup`):

1. Stay within that session until terminal (`Finish`/`Abort`).
2. If history shows `more lines` and the step requires complete evidence, emit `Read` first.
3. Only emit `Finish` after required pages/slices are retrieved for the current objective.
4. Do not start unrelated tool/session work before closing the current session.

### Immediate refinement direction

- Tighten `__continue__` prompt constraints with explicit decision table:
  - `more lines` present + needed evidence missing -> `Read`
  - no remaining pages OR evidence complete -> `Finish`
- Add one canonical few-shot for pagination (`@N ... offset=...` -> `Read`).
- Add agent-level post-parse guard: if model emits `Finish` while latest relevant archive signals `more lines` and objective is unresolved, force one bounded `Read` hop.

### Status update (implemented)

Completed in code and validated in logs:

- `ChooseClickUpAction` prompt now includes an explicit archive-ref retrieval protocol and strict JSON output discipline.
- Parse retries are explicitly configured for the ClickUp client path.
- Agent host adds a post-parse safety-net hop when a large archive appears to end prematurely without Read.
- Prompt history formatting was hardened in both host projection and ClickUp prompt templates, fixing prior entry-glue artifacts (`...management@9...`, `..."@12...`, `cat -n @18cat -n @18...`).
- ClickUp action labels now include runtime qualifiers (e.g., `ListTasks(list_id=...)`) for better disambiguation.

### Next improvements (token efficiency + retrieval discipline)

Observed in logs: retrieval works, but token usage is still high due to repeated replay of the same archive views.

TODOs:

1. **Suppress duplicate read projections in prompt history**
   - If the same `(archive_ref, grep, offset, limit)` view was already inlined, omit repeat body entries (or replace with a tiny marker).

2. **Add deterministic read-complete guard**
   - If latest read result has `has_more=false`, reject identical follow-up Read of the same view in the same execution path.

3. **Trim executor conversation window**
   - For `ChooseClickUpAction` hops, pass a compact recent/relevant context window instead of replaying full long history each hop.

4. **Pass compact resolved-ID state**
   - Include small structured state (resolved `team_id`/`space_id`/`list_id`) so follow-up turns avoid rediscovery chains.

5. **Keep retrieval generic, not intent-specific branching**
   - Continue improving evidence-sufficiency policy (read until sufficient) rather than adding per-intent hardcoded logic.

6. **Address cross-turn reuse latency regression (same context, similar query)**
   - Repro observed in CLI (`cargo agent-platform chat --agent clickup-agent`) within the same `context_id`:
     - Turn 1: `which tasks are in progress?` returns a completed `Read` on `@18` with `has_more=false` (`lines 201-209 of 209`).
     - Turn 2: `which tasks are in to do?` takes longer and ends with session `{ "status": "finished" }`, despite prior full list already being available in provenance for the same context.
   - This suggests follow-up turns may still trigger expensive plan/selection loops or broad context replay instead of fast reuse from archived evidence.
   - Must investigate and fix at some point: for follow-up status filters over an already materialized task list, prefer deterministic reuse (cached archive evidence + narrow filtering) over re-discovery paths.

---

## Handling huge tool outputs (e.g., 500 ClickUp tasks)

Never inject full blobs into prompt history.

Adopt this retrieval sequence:

1. **Aggregate first**
   - query counts/grouping via extrospection/introspection
   - identify hotspots (status, assignee, due-date buckets)

2. **Drilldown second**
   - use `retrieve_ref` with `summary` projection
   - only switch to `detail` for selected refs

3. **Slice third**
   - line/range cite (`@N:Lx-Ly`) for specific claims
   - paginate narrow windows

4. **Promote final facts**
   - write only validated key facts into run capsule
   - keep large raw data in provenance only

This yields low token load and high auditability.

---

## Refactored memory layers

1. **Layer 0 — Recency context**
   - small `conversation_history` projection for turn continuity

2. **Layer 1 — Provenance retrieval layer (primary memory read path)**
   - query + drilldown + cited evidence extraction

3. **Layer 2 — Typed execution capsule (source-of-truth for actions)**
   - validated IDs/constraints used for delegated writes

4. **Layer 3 — Optional cognitive memory tools (`memory/*`)**
   - user preferences, long-horizon heuristics, non-critical recall
   - never authoritative for write-critical IDs without validation

---

## Integration plan (minimal disruption)

### Phase A (now)

- Keep current provenance + citation contract.
- Reduce default context projection budgets for delegated calls.
- Force provenance retrieval for large/tool-heavy contexts.

### Phase B

- Add coordinator retrieval helpers (FSM wrappers):
  - `queryProvenanceOnce(args)`
  - `retrieveRefOnce(refId, projection)`
- Internal flow remains: `Open -> Send -> Read -> Finish`.
- Compose helper pipeline:
  - query -> hotspot selection -> retrieve_ref(summary/detail) -> cite extraction
- Normalize outputs into compact "evidence cards" for planner/synthesizer.

### Phase C

- Add typed promotion pipeline:
  - `extract -> validate -> promote -> delegate(capsule)`
- Gate external writes behind promoted facts + idempotency key.

### Phase D

- Use `memory/*` only for advisory long-term memory, not as duplicate provenance store.

---

## Non-goals

- Building a second canonical event/memory DB duplicating provenance.
- Embedding every tool output by default.
- Using semantic recall directly for write-critical actions without validation.

---

## Success criteria

- Delegated prompts shrink significantly on repeated turns.
- Large tool outputs are handled via drilldown, not transcript bloat.
- Write operations depend on typed promoted facts, not raw history parsing.
- Every key claim/action can be traced to explicit citations.
