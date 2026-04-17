# OpenTelemetry metrics inventory (workspace)

Authoritative list of **exported OTLP metric names** (OpenTelemetry/Prometheus), where they are defined, and their **operational purpose**. Dynamic values live in **attributes**, not in names.

For implementation patterns, see [otel-metrics-instrumentation-guide.md](./otel-metrics-instrumentation-guide.md).

---

## `baml_rt` meter — [`crates/baml-rt-observability/src/metrics.rs`](../crates/baml-rt-observability/src/metrics.rs)

Core runtime: A2A, tools (via QuickJS), LLM, QuickJS bridge, live stream, ONNX, SSE latency.

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt.a2a.request_total` | Counter | A2A JSON-RPC handler completions by `method`, `result`, `stream`. |
| `baml_rt.a2a.request_duration_ms` | Histogram | End-to-end A2A handler latency for those labels. |
| `baml_rt.a2a.error_total` | Counter | Classified A2A failures by `method`, `error_type`, `stream`. |
| `baml_rt.a2a.stream.chunk_total` | Counter | Total SSE/stream chunks emitted (weighted by chunk count per flush). |
| `baml_rt.a2a.stream.chunk_count` | Histogram | Distribution of chunk counts per stream completion (`method`). |
| `baml_rt.tool.invocation_total` | Counter | **Canonical** host tool completion by `tool`, `result` (QuickJS bridge). |
| `baml_rt.tool.invocation_duration_ms` | Histogram | Wall time for that invocation path. |
| `baml_rt.a2a.worker.handle_total` | Counter | QuickJS worker-thread handle completions by `result`. |
| `baml_rt.a2a.worker.handle_duration_ms` | Histogram | Time to complete worker handle. |
| `baml_rt.a2a.task_store.operation_total` | Counter | Surreal task subgraph store ops by `operation`, `result`. |
| `baml_rt.a2a.task_store.operation_duration_ms` | Histogram | Latency of those operations. |
| `baml_rt.a2a.event_poll.cycle_total` | Counter | One increment per [`EventDispatcher::poll_and_deliver`](../crates/baml-rt-a2a/src/event_dispatcher.rs) sweep (all producers). |
| `baml_rt.a2a.event_poll.cycle_duration_ms` | Histogram | Wall time for that full sweep. |
| `baml_rt.a2a.event_poll.producer_outcome_total` | Counter | Per-producer poll by bounded `producer_key` and `outcome` (`empty`, `poll_error`, `validation_error`, `delivery_error`, `partial_rejection`, `success`). |
| `baml_rt.a2a.event_poll.producer_duration_ms` | Histogram | Latency per producer within a sweep (same attributes as `producer_outcome_total`). |
| `baml_rt.a2a.event_poll.events_processed_total` | Counter | Events in a non-empty batch (`producer_key` only; adds batch size per outcome). |
| `baml_rt.quickjs.invoke_total` | Counter | QuickJS invoke path by `mode`, `result` (stream vs sync). |
| `baml_rt.quickjs.invoke_duration_ms` | Histogram | Latency of QuickJS invoke. |
| `baml_rt.a2a.live_stream.event_total` | Counter | Low-cardinality milestones on HTTP `message.sendStream` (`event`). |
| `baml_rt.a2a.live_stream.phase_duration_ms` | Histogram | Time between phases on live stream (`phase`). |
| `baml_rt.a2a.sse.first_data_from_stream_ms` | Histogram | Stream open → first bus chunk mapped to SSE `data:`. |
| `baml_rt.a2a.sse.ttfb_from_handler_entry_ms` | Histogram | HTTP handler entry → first SSE data (full TTFB). |
| `baml_rt.llm.call_total` | Counter | LLM calls by `function`, `client`, `model`, `result`. |
| `baml_rt.llm.call_duration_ms` | Histogram | LLM round-trip latency. |
| `baml_rt.llm.prompt_bytes` | Histogram | Serialized prompt size (provenance subscriber path). |
| `baml_rt.llm.tokens_in_total` | Counter | Input tokens (when usage available). |
| `baml_rt.llm.tokens_out_total` | Counter | Output tokens (when usage available). |
| `baml_rt.onnx.inference_total` | Counter | Embedding/rerank ONNX batches by `operation`. |
| `baml_rt.onnx.wait_ms` | Histogram | Queue/wait time before ONNX run. |
| `baml_rt.onnx.run_ms` | Histogram | ONNX session run time. |
| `baml_rt.onnx.wait_to_run_ratio` | Histogram | `wait_ms / run_ms` (saturation signal). |
| `baml_rt.onnx.wait_dominant_total` | Counter | Count where wait ≥ run (contention). |

---

## `baml_rt_provenance` meter — same file

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_provenance.event.write_total` | Counter | Provenance graph writes by `event_kind`, `result`. |
| `baml_rt_provenance.event.write_duration_ms` | Histogram | Write latency. |
| `baml_rt_provenance.sequence.render_total` | Counter | Mermaid/sequence renders by `scope`, `nodes_bucket`. |
| `baml_rt_provenance.sequence.render_duration_ms` | Histogram | Render latency. |
| `baml_rt_provenance.read.operation_total` | Counter | Heavy Surreal-backed graph reads by `operation`, `result`. |
| `baml_rt_provenance.read.duration_ms` | Histogram | Latency of those reads. |

`operation` values include `export_by_context`, `export_by_task`, `list_contexts` (see [`graph_export/mod.rs`](../crates/baml-rt-provenance/src/graph_export/mod.rs)).

---

## `baml_rt_core` meter — [`crates/baml-rt-core/src/effect_metrics.rs`](../crates/baml-rt-core/src/effect_metrics.rs)

Effect bus and subscriber fan-out (liveness + provenance subscribers).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_core.effect_emit.process_duration_ms` | Histogram | End-to-end `process_effect` by `event.variant`. |
| `baml_rt_core.effect_emit.subscriber_notify_total` | Counter | Subscriber invocations by `event.variant`, `dispatch.mode`, `result`. |
| `baml_rt_core.effect_emit.subscriber_duration_ms` | Histogram | Per-subscriber `on_effect` latency (same attributes). |
| `baml_rt_core.bus.emit_envelope_duration_ms` | Histogram | `Bus::emit` fan-out by `envelope.subscriber_bucket`. |

---

## `baml_rt_api` meter — [`crates/baml-rt-api/src/metrics.rs`](../crates/baml-rt-api/src/metrics.rs)

HTTP API surface (embedded in runner or standalone).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_api.http.request_total` | Counter | API requests by low-cardinality `route`, `result`. |
| `baml_rt_api.http.request_duration_ms` | Histogram | Request latency. |
| `baml_rt_api.conversation_history.phase_duration_ms` | Histogram | Snapshot/delta build phases (`phase`). |
| `baml_rt_api.conversation_history.payload_bytes` | Histogram | Response body size by `event` kind. |
| `baml_rt_api.conversation_history.item_count` | Histogram | Items per page/snapshot by `event`. |

**Note:** Control-plane routes `post_deploy`, `post_undeploy`, `get_deployments`, `post_migrate`, and agent `post_dispatch` record `route` + `result`. Config CRUD and static `/healthz` / OpenAPI JSON remain without per-route `record_request` unless extended (see [Gap review](#operational-gap-review)).

---

## `baml_rt_repository` meter — [`crates/baml-rt-repository/src/metrics.rs`](../crates/baml-rt-repository/src/metrics.rs)

Agent package repository (publish, search, blobs).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `repository.publish.total` | Counter | Publish attempts by `agent_name`, `result`. |
| `repository.publish.duration_ms` | Histogram | Publish duration. |
| `repository.fork.total` | Counter | Fork lineage operations. |
| `repository.search.total` | Counter | Search requests by `result_count_bucket`. |
| `repository.search.duration_ms` | Histogram | Search latency. |
| `repository.blob.read_total` | Counter | Blob reads by `result`. |
| `repository.blob.write_total` | Counter | Blob writes by `result`. |
| `repository.hash.duration_ms` | Histogram | Content hash compute time. |

---

## `baml_rt_tools` meter — [`crates/baml-rt-tools/src/metrics.rs`](../crates/baml-rt-tools/src/metrics.rs)

Registry lifecycle and session FSM timings. **Tool run completion** totals use `baml_rt.tool.invocation_*` only (see guide).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_tools.tool.registration.total` | Counter | Tool registered into registry (`tool`). |
| `baml_rt_tools.tool.session.open.total` | Counter | Session opens (`tool`). |
| `baml_rt_tools.tool.session.operation.duration_ms` | Histogram | FSM ops: `open`, `send`, `read`, `finish`, `abort` (`operation`). |

---

## Grafana / local stack

Starter dashboard: `observability/grafana/dashboards/agent-platform-overview.json`.
Prometheus summary helper: `just otel-summary <window>`.

---

## Operational gap review

Structured gaps: **what operators care about**, **what exists today**, **what to add or wire**, **priority**.

### P1 — Host control plane visibility

| Gap | Today | Recommendation |
|-----|--------|----------------|
| **Deploy / undeploy / list deployments HTTP** | **Done:** `post_deploy`, `post_undeploy`, `get_deployments`, and `post_migrate` call `record_request`; `post_dispatch` is instrumented. | — |
| **Config / OpenAPI / health** | Config CRUD and static routes may still lack `record_request`. | Audit `router.rs` vs handlers; optional Axum middleware for uniform `baml_rt_api.http.*` (see OTEL_COMPLIANCE “gaps”). |

### P1 — Event producer pipeline

| Gap | Today | Recommendation |
|-----|--------|----------------|
| **Producer poll loop** | **Done:** `baml_rt.a2a.event_poll.*` from [`EventDispatcher::poll_and_deliver`](../crates/baml-rt-a2a/src/event_dispatcher.rs) (cycle + per-producer outcomes and durations; `events_processed_total` for batch sizes). | — |
| **Dispatch outcomes vs subscriptions** | Partially visible via A2A if agents receive; not aggregated per producer. | Optional: `event_dispatch.subscriber_matched_total` / `no_match_total` per `producer_key` pattern (careful with cardinality). |

### P2 — Provenance read path

| Gap | Today | Recommendation |
|-----|--------|----------------|
| **Graph export / Surreal reads** | **Done for graph exporter paths:** `baml_rt_provenance.read.*` on `export_by_context`, `export_by_task`, `list_contexts`. Other heavy reads can add `operation` values the same way. | — |

### P2 — Binaries without OTLP wiring

| Gap | Today | Recommendation |
|-----|--------|----------------|
| **`task-daemon`** | Uses tracing; no workspace `metrics.rs` pattern for OTLP. | If operators run it in production: optional `init_tracing` + shared meter for poll/extract/sink outcomes (same guide). |
| **Cluster / internal A2A** | No dedicated `baml_rt.cluster.*` metrics found. | If multi-runner routing is operational: request counts and error rates for cluster forwards (separate from stdio A2A). |

### Covered well (no gap)

- **A2A handler** request/error/stream/tool/LLM/onnx (above tables).
- **Effect bus** (`baml_rt_core.effect_*`, `bus.emit_envelope_*`).
- **Repository** publish/search/blob/hash.
- **Tool completion** (`baml_rt.tool.invocation_*`) without duplicating `baml_rt_tools` execution totals.

---

## Changelog

- Keep this file in sync when adding or removing instruments in any `metrics.rs` or `effect_metrics.rs`.
- Event poll (`baml_rt.a2a.event_poll.*`), provenance read (`baml_rt_provenance.read.*`), and API control/dispatch `record_request` routes wired per [Gap review](#operational-gap-review) (deploy, migrate, dispatch; provenance graph export/list).
