# ONNX Async Migration Validation Matrix

Date: 2026-04-03

Purpose: validate semantic parity + local runtime performance after moving ONNX inference off async workers.

---

## Preconditions (disable OTEL test noise)

```bash
export OTEL_SDK_DISABLED=true
export OTEL_TRACES_EXPORTER=none
export OTEL_METRICS_EXPORTER=none
export OTEL_LOGS_EXPORTER=none
export OTEL_EXPORTER_OTLP_ENDPOINT=""
```

---

## A) Compile / smoke gates

```bash
cargo check -p baml-rt-embedding -p baml-rt-provenance
cargo test -p baml-rt-embedding --lib --no-run
cargo test -p baml-rt-provenance --lib --no-run
```

Pass criteria:
- all commands succeed
- no new warnings in changed paths

---

## B) Drift semantics regression (focused)

```bash
cargo test -p baml-rt-embedding --lib
cargo test -p baml-rt-provenance --lib effect_subscriber -- --nocapture
```

Pass criteria:
- tactical/plan/citation drift tests pass
- no unexpected severity flips in existing assertions

---

## C) Optional CI-parity confidence

```bash
just test
```

Pass criteria:
- no new flakes attributable to drift migration

---

## D) Local performance sanity (no real LLM/network)

Use the dedicated provenance benchmark:

```bash
cargo bench -p baml-rt-provenance --bench drift_llm_completed
```

What this bench measures:
- `ProvenanceEffectSubscriber::on_effect(EffectEvent::LlmCompleted)`
- local mock embedding provider with controlled per-call delay
- no external LLM/network latency

Bench cases:
- `single_event`
- `batch_events/4`
- `batch_events/16`
- `batch_events/64`

Pass criteria (sanity):
- benchmark runs to completion
- no regressions versus previous baseline on same machine/profile

Notes:
- This validates runtime-side behavior in isolation.
- It does **not** replace end-to-end latency checks with real providers.

---

## E) Runtime observability checks

While running representative workloads, inspect logs for:
- `ONNX embed_batch offloaded` (debug)
- `ONNX rerank offloaded` (debug)
- `ONNX citation drift scoring offloaded` (debug)

Look at:
- `wait_ms`: queue/backpressure on inference semaphore
- `run_ms`: actual blocking compute duration

Interpretation:
- high `wait_ms` + low `run_ms` => semaphore bottleneck (consider tuning slots)
- high `run_ms` => model/inference CPU dominates (expected under heavy load)

---

## Run log

- [ ] A) Compile / smoke gates
- [ ] B) Drift semantics regression
- [ ] C) Optional CI-parity confidence
- [ ] D) Local performance sanity benchmark
- [ ] E) Runtime observability checks
