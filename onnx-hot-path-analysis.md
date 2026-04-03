# ONNX Hot-Path Analysis (Drift / Semantic Scoring)

Date: 2026-04-03  
Scope: `baml-rt-embedding`, `baml-rt-provenance`, `baml-rt-a2a`, `baml-rt-core`

---

## Executive summary

ONNX models are used for **embedding/rerank drift scoring** in the provenance effect pipeline (primarily `LlmCompleted`), not as part of request-time LLM interceptor gating. Cold-start is already mitigated via explicit warm-up, but there are still hot-path opportunities to reduce tail latency and contention.

Primary levers:
- offload CPU inference from async workers,
- remove duplicate work (embeddings + store reads),
- optionally parallelize embedding + rerank once offloaded,
- decide whether drift must be synchronous with provenance completion.

---

## Where ONNX is used

- Embeddings (bi-encoder): `crates/baml-rt-embedding/src/provider.rs`
  - `FastEmbedProvider` (GTE-base by default)
- Cross-encoder rerank: `crates/baml-rt-embedding/src/reranker.rs`
  - `FastRerankProvider` (JINA v1 turbo en by default)
- Runtime drift usage: `crates/baml-rt-provenance/src/effect_subscriber.rs`
  - `compute_drift`, `compute_plan_drift`, citation drift scoring
- Warm-up wiring: `crates/baml-rt-a2a/src/a2a_transport.rs`
  - `subscriber.warm_drift_models().await`

---

## Findings (combined)

Below are **your findings** plus **additional observations** from code review.

### 1) CPU-bound ONNX on the async runtime *(your finding)*

`embed_batch` / `score_pair` are synchronous and called inside async drift paths. This can block Tokio worker threads and inflate tail latency under load.

**Why it matters:** inference can take tens of ms (or worse under contention), starving unrelated async tasks.

**Action:** run inference in `spawn_blocking` (or dedicated bounded pool) for steady-state calls, not just model initialization.

---

### 2) Fewer redundant bi-encoder forwards *(your finding)*

`response_text` can be embedded more than once across tactical + plan drift paths for the same completion.

**Why it matters:** duplicated ONNX forwards are pure overhead.

**Action:** cache response embedding for the lifetime of one `compute_drift` execution (or fuse batches where practical).

---

### 3) Serialization vs parallelism *(your finding)*

There are separate mutex-guarded model instances (bi-encoder and reranker), but computation is effectively sequenced in current flow.

**Why it matters:** GTE and JINA are independent signals and can run concurrently when both are needed.

**Action:** once inference is offloaded, run embedding + rerank concurrently (`tokio::join!` over blocking tasks / pool jobs).

---

### 4) Duplicate async I/O for context reads *(your finding)*

On `LlmCompleted`, `conversation_context_with_task` is read inside citation scoring and again afterwards for resolved citations.

**Why it matters:** duplicate store reads on a hot path add avoidable latency and DB pressure.

**Action:** fetch once and pass through both consumers.

---

### 5) Warm-up correctness *(your finding + confirmed)*

`warm_drift_models()` is correctly used to avoid first-turn initialization stalls.

**Why it matters:** skipping warm-up reintroduces 10–40s first-load stalls from ONNX graph init.

**Action:** keep warm-up mandatory in all runtime boot paths that wire provenance drift.

---

### 6) Model/EP tradeoffs *(your finding + confirmed)*

The embedding crate already documents/evaluates smaller and quantized alternatives (see ignored eval tests / model variants).

**Why it matters:** throughput and quality tradeoff is real; hot-path SLOs may prefer smaller/INT8 defaults for some deployments.

**Action:** add explicit runtime config profile(s): `quality`, `balanced`, `throughput`.

---

### 7) Design-level sync vs deferred drift *(your finding)*

Today drift is attached in provenance completion flow. Deferring drift (async enrichment/update) would remove ONNX from critical user path.

**Why it matters:** largest possible latency reduction, but changes consistency semantics.

**Action:** product decision required: strict immediate drift vs eventual drift annotation.

---

### 8) Additional observation: possible duplicate LLM provenance path

In `a2a_transport.rs`, comments say LLM interceptor registration is intentionally omitted for provenance duplication concerns, but code registers LLM interceptor alongside tool interceptor.

**Why it matters:** may cause extra synchronous completion work in parallel to effect-subscriber provenance path.

**Action:** verify intended behavior and remove duplicate LLM provenance completion path if unnecessary.

---

## Prioritized remediation plan

### P0 (high impact / low-medium risk)
- [x] Offload steady-state ONNX inference (`embed_batch`, `score_pair`) from async worker threads.
- [x] Reuse one `conversation_context_with_task` fetch in `LlmCompleted` flow.
- [x] Tactical path migrated using shared extraction/scoring helpers (`tactical_drift_texts`, `score_drift_from_embeddings`) to avoid rule drift while moving inference off async workers.
- [ ] Optional follow-up: response-embedding dedupe across tactical + plan (explicitly deferred for now to avoid semantic coupling).

### P1 (high impact / medium risk)
- [ ] Parallelize bi-encoder + cross-encoder scoring when both required.
- [ ] Validate/remove duplicate LLM provenance completion path (interceptor vs effect subscriber).

### P2 (product/config level)
- [ ] Add model profile configuration (`quality`/`balanced`/`throughput`).
- [ ] Decide on synchronous vs deferred drift attachment semantics.

---

## Success criteria

Track before/after on representative workloads:
- `turn_total_ms`, `llm_total_ms`, `llm_calls_count`
- drift scoring duration per `LlmCompleted`
- async runtime health (Tokio busy/latency indicators)
- provenance store read count per completion

Expected outcomes:
- lower p95/p99 turn latency under concurrent load,
- fewer duplicate drift/provenance operations,
- unchanged drift detection correctness for selected model profile.

---

## Progress update (2026-04-03)

Completed in code:
- Async ONNX offload adapters added in provenance subscriber (`spawn_blocking` + bounded semaphore).
- Plan drift embeddings/rerank moved off async worker path.
- Citation drift scoring moved off async worker path.
- Tactical drift moved to shared helper-based flow to preserve extraction semantics while offloading inference.
- Single conversation-context snapshot reuse for `LlmCompleted` citation scoring + resolved citations.
- Local benchmark added: `cargo bench -p baml-rt-provenance --bench drift_llm_completed`.

Current benchmark signal:
- No meaningful regression detected in local criterion runs (single and batch cases within noise).
