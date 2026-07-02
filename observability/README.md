# Observability Stack (OTel + Prometheus + Tempo + Grafana)

This stack is generic for **any agent(s)** in this repo (single or multiple running concurrently).

## What you get

- **OTel Collector**: receives OTLP metrics/traces from runner(s)
- **Prometheus**: stores metrics
- **Tempo**: stores traces
- **Grafana**: live visor for metrics + traces

## Start stack

From repo root:

```bash
cd observability
docker compose up -d
```

Open:
- Grafana: http://localhost:3000 (`admin` / `admin`)
- Prometheus: http://localhost:9090

## Point agent runner(s) to collector

Set these env vars when starting `baml-agent-runner` (or any process using `baml-rt-observability`):

```bash
export OTEL_TRACES_EXPORTER=otlp
export OTEL_METRICS_EXPORTER=otlp
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
```

### Multi-agent / multi-process labeling (recommended)

To separate concurrent runners/agents cleanly, set resource attributes per process:

```bash
export OTEL_SERVICE_NAME=agentium-runner
export OTEL_RESOURCE_ATTRIBUTES="deployment.environment=local,service.namespace=agentium,service.instance.id=$(hostname)-$$"
```

Use a different `service.instance.id` per process. The runtime also emits the canonical per-metric identity labels (`ingress_service_instance_id`, `serving_service_instance_id`, `target_service_instance_id`) regardless of resource-attr overrides — see [Pilot observability contract](#pilot-observability-contract) below.

Then run your process, e.g.:

```bash
cargo run -p agentium -- serve
```

## Grafana dashboard

A pre-provisioned dashboard is included:

- **Agent Platform / Agent Platform Overview**

Template variables: `agent_package` and `agent_instance_id` (both default to "All"). They filter every panel that includes an `agent_package` / `agent_instance_id` label — i.e. the Cluster & Ingress panels and the A2A panels. LLM, tool, and ONNX panels are not agent-scoped and are not affected by these variables. Runner identity is exposed as per-panel legends (`ingress_service_instance_id`, `serving_service_instance_id`, `target_service_instance_id`) rather than as a dashboard-wide filter, because a single runner variable cannot disambiguate ingress / serving / target roles across panels.

Panel groups:

- **Cluster & Ingress** — ingress HTTP rate/latency by agent route, serving A2A rate and errors by agent + runner, forwarded-vs-local ingress split, cluster A2A forward rate and duration (split by `ingress_service_instance_id` and `target_service_instance_id`).
- **A2A / LLM / Tools / ONNX** — A2A request latency (split by `serving_service_instance_id`), LLM calls/sec, duration, prompt bytes, tokens in/out; tool call rate + latency; ONNX wait/run/ratio and wait-dominant events.

The dashboard links out to Grafana Explore for trace correlation against the provisioned Tempo datasource. See [otel-trace-instrumentation-guide.md § Cross-runner A2A forwarding](../docs/reference/otel-trace-instrumentation-guide.md#cross-runner-a2a-forwarding) for TraceQL recipes.

## Pilot observability contract

The shipped runner emits these OpenTelemetry **resource attributes** on every signal: `service.name=agentium-runner`, `service.namespace=agentium`, `service.instance.id=<runner identity>`, `deployment.environment=<Helm value>`, `k8s.namespace.name=<pod namespace>`. Full derivation details live in [docs/reference/metrics-inventory.md § Runner identity labels](../docs/reference/metrics-inventory.md#runner-identity-labels).

- **Resource attributes are not automatically Prometheus labels.** The collector config shipped in this directory ([`otel-collector-config.yaml`](./otel-collector-config.yaml)) runs a `transform/runner_identity` processor that promotes `service.instance.id` → datapoint attr `service_instance_id` and `k8s.namespace.name` → `k8s_namespace_name`. Operators running their own collector must replicate that transform or rely on the explicit per-metric identity labels below.
- **Explicit identity labels** on metrics — available regardless of collector transforms:
  - `ingress_service_instance_id` on `baml_rt_api_http_request_*` (agent routes) and `baml_rt_cluster_a2a_forward_*`
  - `serving_service_instance_id` on `baml_rt_a2a_request_*`, `baml_rt_a2a_error_total`
  - `target_service_instance_id` on `baml_rt_cluster_a2a_forward_*` (may be the literal `unknown` when the cluster resolver fallback fires)
- **`forwarded` is advisory.** On public `/agents/...` routes the `forwarded=true` label / span attribute is derived from the W3C baggage key `ingress_service_instance_id`. Any HTTP client can set the same baggage header; treat `forwarded` as a telemetry slice, not an authorization boundary. See the trace guide's [Cross-runner A2A forwarding](../docs/reference/otel-trace-instrumentation-guide.md#cross-runner-a2a-forwarding) section.
- **Authoritative label contract** for each metric family lives in [docs/reference/metrics-inventory.md](../docs/reference/metrics-inventory.md).

## Notes

- Prometheus metric names are normalized to underscore style (`.` → `_`). Docs use the dotted OTLP form; the dashboard JSON uses the Prometheus-normalized form.
- If you run many concurrent processes under the same collector, the shipped dashboard slices by `agent_package` / `agent_instance_id` (template variables) and by the per-metric runner identity labels `ingress_service_instance_id` / `serving_service_instance_id` / `target_service_instance_id` (panel legends). For custom dashboards the shipped `transform/runner_identity` processor also exposes `service_instance_id` and `k8s_namespace_name` as datapoint attributes; `service.name` is a resource attribute and is only reachable as a metric label by extending that transform or joining on Prometheus' `target_info` series.
