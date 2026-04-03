# ONNX Async Inference Migration Plan

Date: 2026-04-03  
Owner: Runtime / Provenance  
Status: In progress

Related:
- `onnx-hot-path-analysis.md`
- `onnx-hot-path-implementation-checklist.md`

---

## Objective

Remove CPU-bound ONNX inference from async hot paths (`LlmCompleted` drift processing) **without changing drift semantics**.

Primary outcomes:
1. No direct sync `embed_batch` / `score_pair` calls on Tokio worker threads in async paths.
2. Preserve current tactical/plan/citation drift scoring behavior.
3. Improve p95/p99 latency and reduce runtime starvation under burst.

---

## Scope

### In scope
- `crates/baml-rt-provenance/src/effect_subscriber.rs`
- `crates/baml-rt-embedding/src/assessment.rs`
- Optional: small internal helper module under provenance for async inference adapters.

### Out of scope
- Public trait redesign (`EmbeddingProvider`, `RerankProvider`) at this stage.
- Model swaps/profile policy changes.
- Deferred drift persistence redesign.

---

## Constraints / Guardrails

- Keep tactical text derivation semantics aligned with current `score_drift`.
- Do **not** hold `DashMap` mutable guards across `.await`.
- Bound blocking task fan-out (semaphore).
- Preserve existing event schemas and provenance payload fields.
- Maintain current citation resolution behavior.

---

## Execution Plan

## Phase 0 — Baseline and instrumentation

Status: ✅ done / ⏳ ongoing

- [x] Disable OTEL exporters for CI and `just test` to reduce noise/flakes.
- [x] Remove duplicate conversation context read in `LlmCompleted` path (single snapshot reuse).
- [ ] Capture baseline timings for drift path (`LlmCompleted`) before migration.

Notes:
- Completed commits:
  - `chore(test): disable OTEL exporters for CI and just test runs`
  - `perf(provenance): reuse single conversation-context snapshot for llm completion drift`

---

## Phase 1 — Shared async inference adapters (provenance-local)

Status: ✅ done

### Deliverables
- [x] Add async helper for embedding batch offload:
  - accepts `Arc<dyn EmbeddingProvider>` + owned `Vec<String>`
  - runs via `spawn_blocking`
  - converts to `Vec<&str>` inside closure
- [x] Add async helper for rerank offload:
  - accepts `Arc<dyn RerankProvider>` + owned `String` pair
  - runs via `spawn_blocking`
- [x] Add bounded concurrency semaphore for inference tasks.
- [x] Add helper-level telemetry (`wait_ms`, `run_ms`, `queue/contention`).

### Design notes
- Acquire semaphore permit on async side before spawning.
- Keep helpers private to provenance path for now.
- Use explicit inference error type (`InferenceError`) to keep embed/rerank failures distinct.

---

## Phase 2 — Plan + citation path migration (mechanical)

Status: ⏳ in progress

### Plan drift call sites to migrate
- [x] `compute_plan_drift` response embedding
- [x] `compute_plan_drift` pre-plan user message embedding
- [x] `on_intent_resolved` intent embedding
- [x] `on_plan_generated` step batch embeddings
- [x] `compute_plan_drift` rerank score

### Citation path migration
- [x] Offload citation embedding/similarity compute via async helper path.

### Concurrency correctness
- [x] Ensure no `plan_trackers` mutable guard is held across `.await`.
- [x] Restructure read/clone/await/update phases where necessary.

### Acceptance
- [ ] No direct sync inference call remains in these async paths.
- [ ] Existing tests pass unchanged (modulo timing).

---

## Phase 3 — Tactical drift migration with semantic lock

Status: ✅ done (implementation), ⏳ pending validation

### Goal
Move tactical inference off async workers **without duplicating tactical rules**.

### Refactor in embedding crate
- [x] Add shared helper(s) to avoid re-implementing tactical extraction logic in subscriber:
  - `tactical_drift_texts(...) -> Option<(String, String)>`
  - `score_drift_from_texts(...)`
  - `score_drift_from_embeddings(...)`
- [x] Keep existing `score_drift(...)` behavior as source of truth.

### Provenance tactical integration
- [x] Use shared extraction helper to derive texts.
- [x] Run embeddings through async adapter.
- [x] Use shared classify/preview logic so semantics match current output.

### Acceptance
- [ ] Tactical scores/severity match prior behavior in regression fixtures.
- [x] No sync tactical embedding on async worker threads.

---

## Phase 4 — Validation and rollout

Status: ⏳ pending

### Functional regression
- [ ] Drift fixture tests pass.
- [ ] Provenance LLM completion tests pass.
- [ ] No schema/output regressions in persisted drift fields.

### Performance validation
- [ ] Compare before/after p50/p95/p99 for representative E2E turns.
- [ ] Verify reduction in async runtime starvation symptoms under concurrency.
- [ ] Confirm bounded blocking queue behavior under burst.

### Rollback criteria
- [ ] Any severity-classification drift unexplained by numerical noise.
- [ ] Significant increase in dropped/empty drift sections.
- [ ] Throughput regression or blocking queue saturation without latency benefit.

---

## Open decisions

1. Semaphore size default (global inference slots).
   - Proposal: start small (e.g. 4), tune with load metrics.

2. Single vs split semaphore (embed vs rerank).
   - Proposal: single initially; split only if starvation observed.

3. Helper placement.
   - Proposal: keep in provenance subscriber/module first; extract later if reused.

---

## Progress log

- 2026-04-03
  - Created migration plan.
  - OTEL disabled in CI/`just test` for cleaner test signal.
  - Implemented single conversation-context snapshot reuse in `LlmCompleted` path.
  - Added provenance-local async inference adapters (`embed_batch_async`, `rerank_score_async`) with bounded semaphore and wait/run telemetry.
  - Migrated plan-path embeddings/rerank off async workers (`compute_plan_drift`, `on_intent_resolved`, `on_plan_generated`).
  - Refactored plan drift rerank flow to avoid holding `plan_trackers` mutable guards across awaits.
  - Offloaded citation drift scoring to async blocking adapter path.
  - Added tactical shared helpers in `baml-rt-embedding` and migrated tactical drift inference off async workers without duplicating extraction rules.

---

## Quick checklist (operator view)

- [x] WS-3 duplicate context read removed
- [x] Async inference adapters added
- [x] Plan path migrated
- [x] Citation path migrated
- [x] Tactical path migrated via shared extraction/scoring helpers
- [x] Guard lifetime audit (`DashMap` + await)
- [ ] Perf/regression validation complete
