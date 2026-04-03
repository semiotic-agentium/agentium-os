# ONNX Hot-Path Implementation Checklist

Source analysis: `onnx-hot-path-analysis.md`  
Date: 2026-04-03

---

## Goal

Reduce latency/contention from ONNX drift scoring on `LlmCompleted` hot paths while preserving provenance correctness.

---

## Workstreams

## WS-1: Offload steady-state ONNX inference from async workers (P0) ✅

### Scope
- `crates/baml-rt-provenance/src/effect_subscriber.rs`
- `crates/baml-rt-embedding/src/provider.rs`
- `crates/baml-rt-embedding/src/reranker.rs`

### Tasks
- [x] Wrap synchronous `embed_batch` calls in blocking offload when invoked from async paths.
- [x] Wrap synchronous `score_pair` rerank calls in blocking offload when invoked from async paths.
- [x] Add bounded concurrency guard (semaphore or small pool) to avoid unbounded `spawn_blocking` growth.
- [x] Add tracing fields for inference wait/run time (`onnx_wait_ms`, `onnx_run_ms`).

### Acceptance
- [x] No direct heavy ONNX forwards on Tokio worker threads in `compute_drift` path.
- [ ] Under load, p95 turn latency improves and no runtime starvation symptoms (needs concurrent workload validation).

---

## WS-2: Deduplicate response embedding work (P0) ⏸️ deferred

### Scope
- `crates/baml-rt-provenance/src/effect_subscriber.rs`
- `crates/baml-rt-embedding/src/assessment.rs`
- `crates/baml-rt-embedding/src/plan_assessment.rs`

### Tasks
- [ ] Compute `response_embedding` once per `compute_drift` call.
- [ ] Thread cached response embedding into tactical + plan scoring paths.
- [ ] Keep behavior identical when tactical/plan drift is unavailable.

Notes:
- Deferred intentionally after confirming tactical and plan paths can derive response text differently.
- Prioritized semantic safety over this optimization for now.

### Acceptance
- [ ] One response embedding forward per completion (unless explicitly required otherwise).
- [ ] Drift outputs unchanged in existing tests (except tolerated float noise).

---

## WS-3: Remove duplicate provenance context reads (P0) ✅

### Scope
- `crates/baml-rt-provenance/src/effect_subscriber.rs`

### Tasks
- [x] Fetch `conversation_context_with_task` once in `LlmCompleted` handling.
- [x] Reuse that single result for citation drift + resolved citation extraction.
- [ ] Add trace counter/field for context reads in this path.

### Acceptance
- [x] Exactly one store read for conversation context per `LlmCompleted` event.

---

## WS-4: Parallelize bi-encoder and cross-encoder scoring (P1)

### Scope
- `crates/baml-rt-provenance/src/effect_subscriber.rs`

### Tasks
- [ ] Execute GTE and JINA scoring concurrently when both are needed.
- [ ] Ensure deterministic fallback behavior if one branch fails.
- [ ] Add per-branch timing (`embedding_ms`, `rerank_ms`, `parallel_total_ms`).

### Acceptance
- [ ] Combined drift scoring wall time < sequential baseline when both models active.

---

## WS-5: Verify/remove duplicate LLM provenance completion path (P1)

### Scope
- `crates/baml-rt-a2a/src/a2a_transport.rs`
- `crates/baml-rt-provenance/src/interceptors.rs`
- `crates/baml-rt-provenance/src/effect_subscriber.rs`
- `crates/baml-rt-quickjs/src/baml_collector.rs`

### Tasks
- [ ] Confirm intended single source of truth for LLM completion provenance.
- [ ] Reconcile comment/code mismatch in `wire_provenance_subsystems`.
- [ ] If duplicate, remove/disable redundant path and keep tool interceptor behavior intact.

### Acceptance
- [ ] No duplicate LLM completion provenance writes for same call.

---

## WS-6: Runtime model profile configuration (P2)

### Scope
- `crates/baml-rt-embedding`
- runner config/docs

### Tasks
- [ ] Add profile enum: `quality | balanced | throughput`.
- [ ] Map to model choices (e.g., full vs quantized/smaller).
- [ ] Document expected quality/latency tradeoffs.

### Acceptance
- [ ] Profile switch works via config without code changes.

---

## WS-7: Product decision – strict vs deferred drift attachment (P2)

### Scope
- provenance event semantics + API expectations

### Tasks
- [ ] Decide if drift must be synchronous with completion event.
- [ ] If deferred, design update mechanism (follow-up event or patch).
- [ ] Define consistency guarantees in docs.

### Acceptance
- [ ] Clear contract documented and tested.

---

## Test plan

- [ ] Unit tests for deduped embedding path (WS-2 deferred).
- [x] Integration check for one-context-read invariant (implemented + regression-tested).
- [ ] Load test for concurrent turns measuring p50/p95/p99.
- [x] Regression tests for drift severity/classification outputs (existing suite passes).
- [x] Add local Criterion benchmark for `LlmCompleted` drift path (`drift_llm_completed`).

---

## Observability KPIs

- `turn_total_ms`
- `llm_total_ms`
- `drift_scoring_ms`
- `onnx_wait_ms`
- `onnx_run_ms`
- `provenance_context_reads_per_llm_completed`
- `llm_completion_events_written`

Target: measurable p95 reduction with unchanged correctness.

---

## Current status summary (2026-04-03)

- WS-1: implemented.
- WS-2: deferred (semantic-coupling risk; revisit later).
- WS-3: implemented.
- WS-4/5/6/7: remaining roadmap items.

Next recommended item: WS-5 (verify/remove duplicate LLM provenance completion path), then WS-4 parallelization if needed after real concurrent profiling.
