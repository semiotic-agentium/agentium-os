# OpenTelemetry compliance (baml-rt-api)

This crate follows the patterns in `otel-trace-instrumentation-guide.md` and `otel-metrics-instrumentation-guide.md`. Workspace-wide metric names and purposes: [`docs/metrics-inventory.md`](../../../docs/metrics-inventory.md).

## Trace (spans)

- **Orthogonal spans module**: `src/spans.rs` defines span helpers; handlers do not use `#[tracing::instrument]`.
- **Static span names**: `baml_rt_api.list_agents`, `baml_rt_api.post_a2a` (namespace prefix, low cardinality).
- **Structured fields**: Dynamic data (`agent_package`, `agent_instance_id`) only in span fields, never in span names.
- **Guard pattern**: Each handler creates a span, enters it with a guard, then runs business logic so children propagate correctly.
- **Logging**: `domain_to_problem` catch-all uses `tracing::warn!(error = ?e, ...)` with static message; router uses `tracing::info!(%addr, "HTTP API listening")` (static message, field for addr).

## Metrics

- **Orthogonal metrics module**: `src/metrics.rs` with `record_request(route, result, duration)`.
- **Static metric names**: `baml_rt_api.http.request_total`, `baml_rt_api.http.request_duration_ms`.
- **OnceLock caching**: Instruments are created once via `OnceLock` and reused (no allocation on hot path).
- **Structured attributes**: `route` (list_agents, post_a2a) and `result` (success, error); low cardinality.
- **Counts and durations**: Every request records both counter increment and duration in ms.
- **All outcomes**: Success and error paths call `record_request` with appropriate `result`.

## Gaps

- **HTTP middleware**: No `OtelAxumLayer` or `TraceLayer` in the API router; when the runner embeds the API, the runner may add HTTP middleware. Root request span (e.g. `http.route`) would then be the parent of `baml_rt_api.*` spans.
- **Meter provider**: If no global meter provider is set, OTEL uses a noop provider; metrics are recorded but not exported until the runner (or host) sets up the provider.
