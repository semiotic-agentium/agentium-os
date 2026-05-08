# OpenTelemetry metrics inventory (workspace)

Authoritative list of **exported OTLP metric names** (OpenTelemetry/Prometheus), where they are defined, and their **operational purpose**. Dynamic values live in **attributes**, not in names.

For implementation patterns, see [otel-metrics-instrumentation-guide.md](./otel-metrics-instrumentation-guide.md).

---

## `baml_rt` meter — [`crates/baml-rt-observability/src/metrics.rs`](../crates/baml-rt-observability/src/metrics.rs)

Core runtime: A2A, tools (via QuickJS), LLM, QuickJS bridge, live stream, ONNX, SSE latency.

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt.a2a.request_total` | Counter | A2A JSON-RPC handler completions. Labels: `method`, `agent_package`, `agent_instance_id`, `result`, `stream`, `serving_service_instance_id`. |
| `baml_rt.a2a.request_duration_ms` | Histogram | End-to-end A2A handler latency (same labels as `baml_rt.a2a.request_total`). |
| `baml_rt.a2a.error_total` | Counter | Classified A2A failures. Labels: `method`, `agent_package`, `agent_instance_id`, `error_type`, `stream`, `serving_service_instance_id`. |
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
| `baml_rt.a2a.event_dispatch.no_subscribers_total` | Counter | Published event had no matching subscriptions (`producer_key`). |
| `baml_rt.a2a.event_dispatch.subscriber_delivery_total` | Counter | After attempting delivery to matching subscribers: `subscribers_bucket` (`0` / `1` / `many`), `outcome` (`all_accepted`, `partial_rejection`, `all_rejected`). |
| `baml_rt.cluster.a2a_forward_total` | Counter | Cross-runner HTTP A2A forward completions (ingress side). Labels: `agent_package`, `agent_instance_id`, `result`, `ingress_service_instance_id`, `target_service_instance_id`. `target_service_instance_id` may be the literal `unknown` when the cluster resolver fallback fires. |
| `baml_rt.cluster.a2a_forward_duration_ms` | Histogram | Forward latency (same labels as `baml_rt.cluster.a2a_forward_total`). |
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

`operation` values include graph export/list (`export_by_context`, `export_by_task`, `list_contexts` in [`graph_export/mod.rs`](../crates/baml-rt-provenance/src/graph_export/mod.rs)) and ops UI queries (`ops_query_llm_calls`, `ops_query_tool_calls`, `ops_query_messages`, `ops_query_aggregates`, `ops_query_lifecycle_events` in [`ops_query.rs`](../crates/baml-rt-provenance/src/surreal_store/ops_query.rs)).

---

## `baml_rt_task_daemon` meter — [`crates/baml-rt-observability/src/metrics.rs`](../crates/baml-rt-observability/src/metrics.rs)

Task daemon poll / extract / deliver loop (when OTLP is enabled for the binary).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_task_daemon.run_once.total` | Counter | One iteration per `source_kind`, `result`. |
| `baml_rt_task_daemon.run_once.duration_ms` | Histogram | Wall time for that iteration. |

---

## `baml_rt_core` meter — [`crates/baml-rt-core/src/effect_metrics.rs`](../crates/baml-rt-core/src/effect_metrics.rs)

Effect bus and subscriber fan-out (liveness + provenance subscribers).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_core.effect_emit.process_duration_ms` | Histogram | End-to-end `process_effect` by `event.variant`. |
| `baml_rt_core.effect_emit.subscriber_notify_total` | Counter | Subscriber invocations by `event.variant`, `dispatch.mode`, `subscriber`, `result`. The `subscriber` attribute is the stable identity returned by `EffectSubscriber::name()` (`provenance`, `auto_status`, `live_stream_relay`, …). The `dispatch.mode` attribute is one of `awaitable` (tier-partitioned `LlmCompleted` path, awaited concurrently before emit returns), `background` (tier-partitioned `LlmCompleted` path, spawned detached), or `sequential` (every other event variant; subscribers run inline on the caller's task in registration order). Paired with `result="error"`, `subscriber`, and `event.variant`, this counter is the canonical alert dimension for `EffectSubscriber::on_effect` failures (GitHub #318) — these failures are logged + counted but do not fail the originating emit. |
| `baml_rt_core.effect_emit.subscriber_duration_ms` | Histogram | Per-subscriber `on_effect` latency (same attributes as `subscriber_notify_total`). |

---

## `baml_rt_api` meter — [`crates/baml-rt-api/src/metrics.rs`](../crates/baml-rt-api/src/metrics.rs)

HTTP API surface (embedded in runner or standalone).

| Metric name | Type | Purpose |
|-------------|------|---------|
| `baml_rt_api.http.request_total` | Counter | API requests. Non-agent routes label set: `route`, `result`. Agent routes (`/agents/{package}/{instance}/...`) label set: `route`, `result`, `agent_package`, `agent_instance_id`, `forwarded`, `ingress_service_instance_id`. See [Runner identity labels](#runner-identity-labels) and [forwarded advisory](./otel-trace-instrumentation-guide.md#cross-runner-a2a-forwarding). |
| `baml_rt_api.http.request_duration_ms` | Histogram | Request latency (same per-route label split as `baml_rt_api.http.request_total`). |
| `baml_rt_api.conversation_history.phase_duration_ms` | Histogram | Snapshot/delta build phases (`phase`). |
| `baml_rt_api.conversation_history.payload_bytes` | Histogram | Response body size by `event` kind. |
| `baml_rt_api.conversation_history.item_count` | Histogram | Items per page/snapshot by `event`. |

**Routes:** Deploy/undeploy/list/migrate/dispatch handlers call `record_request`. Config CRUD uses stable `route` labels in [`config_handlers.rs`](../crates/baml-rt-api/src/config_handlers.rs) (e.g. `config_get`, `config_put`, `config_secrets_overview`). Static `/openapi.json`, `/healthz`, `/readyz`, `/diagnose` and nested repository paths are timed in [`router.rs`](../crates/baml-rt-api/src/router.rs) (`get_openapi_json`, `get_healthz`, `get_readyz`, `get_diagnose`, plus `repository_*` via middleware). `/diagnose` reports continuous-readiness signal (`runtime_progress_lag_ms`) distinct from the boot-time `/readyz` gate.

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

## Runner identity labels

The runner emits these OpenTelemetry **resource attributes** on every signal (see [`runner_identity.rs`](../crates/baml-rt-observability/src/runner_identity.rs) and [`otel_env.rs`](../crates/baml-rt-observability/src/otel_env.rs)):

| Resource attr | Value / source |
|---|---|
| `service.name` | `agentium-runner` (override via `OTEL_SERVICE_NAME`). |
| `service.namespace` | `agentium` (override via `OTEL_RESOURCE_ATTRIBUTES`). |
| `service.instance.id` | Per-runner identity. Resolution order: `OTEL_RESOURCE_ATTRIBUTES` override → `POD_NAME` (K8s downward API) → `HOSTNAME` → stable UUID fallback. The Helm chart sets this from `$(POD_NAME)`. |
| `deployment.environment` | Helm: `coalesce(observability.environment, global.environment, "pilot")`. Non-Helm: `OTEL_DEPLOYMENT_ENVIRONMENT` or default `pilot`. |
| `k8s.namespace.name` | `$(POD_NAMESPACE)` when present. |

**Resource attributes are not automatically Prometheus labels.** The local OTel Collector at [`observability/otel-collector-config.yaml`](../observability/otel-collector-config.yaml) runs a `transform/runner_identity` processor that promotes `service.instance.id` → datapoint attr `service_instance_id` and `k8s.namespace.name` → `k8s_namespace_name`. Operators running a different collector must replicate that transform or rely on the **explicit per-metric identity labels** below, which do not depend on any processor:

| Explicit identity label | Present on | Meaning |
|---|---|---|
| `ingress_service_instance_id` | `baml_rt_api_http_request_*` (agent routes), `baml_rt_cluster_a2a_forward_*` | Identity of the runner that first received the public HTTP request. On agent routes, derived from W3C baggage when `forwarded=true`, otherwise the local runner. |
| `serving_service_instance_id` | `baml_rt_a2a_request_*`, `baml_rt_a2a_error_total` | Identity of the runner that actually executed the A2A handler (local runner from the serving side's point of view). |
| `target_service_instance_id` | `baml_rt_cluster_a2a_forward_*` | Identity of the peer runner an ingress forward was routed to. The literal `unknown` is emitted when the cluster resolver fallback fires. |

The `forwarded` label on `baml_rt_api_http_request_*` is **advisory** (derived from W3C baggage on public `/agents/...` routes) — see the trace guide's [cross-runner A2A forwarding](./otel-trace-instrumentation-guide.md#cross-runner-a2a-forwarding) section.

---

## Grafana / local stack

Starter dashboard: `observability/grafana/dashboards/agent-platform-overview.json`.
Prometheus summary helper: `just otel-summary <window>`.

---

## Operational gap review

Prior gap items below are **wired** in tree; extend only when new surfaces ship (new HTTP routes, new heavy Surreal reads, new daemons).

| Area | Coverage |
|------|----------|
| **Control plane HTTP** | Deploy/undeploy/list/migrate/dispatch + config CRUD + OpenAPI/health/ready + repository nested router (`baml_rt_api.http.*`). |
| **Event pipeline** | Poll sweep (`baml_rt.a2a.event_poll.*`) + per-delivery aggregation (`baml_rt.a2a.event_dispatch.*`) from [`event_dispatcher.rs`](../crates/baml-rt-a2a/src/event_dispatcher.rs). |
| **Provenance reads** | Graph export/list + `query_ops` resource kinds (`baml_rt_provenance.read.*`). |
| **Cluster A2A** | HTTP forwards (`baml_rt.cluster.a2a_forward_*`) in [`forward.rs`](../crates/baml-rt-router/src/forward.rs). |
| **task-daemon** | Run loop (`baml_rt_task_daemon.run_once.*`) when the binary exports OTLP. |

### Covered well (baseline)

- **A2A handler** request/error/stream/tool/LLM/onnx (above tables).
- **Effect bus** (`baml_rt_core.effect_*`, `bus.emit_envelope_*`).
- **Repository** publish/search/blob/hash.
- **Tool completion** (`baml_rt.tool.invocation_*`) without duplicating `baml_rt_tools` execution totals.

---

## Changelog

- Keep this file in sync when adding or removing instruments in any `metrics.rs` or `effect_metrics.rs`.
- Event poll, event dispatch, cluster forward, task-daemon run-once, provenance `query_ops` reads, config/static/repository HTTP routes, and graph export/list are inventoried above.
