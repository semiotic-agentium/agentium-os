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
export OTEL_SERVICE_NAME=agent-platform-runner
export OTEL_RESOURCE_ATTRIBUTES="deployment.environment=local,service.namespace=agent-platform,service.instance.id=$(hostname)-$$,agent.package=clickup-agent"
```

Use different `service.instance.id` / `agent.package` values per process.

Then run your process, e.g.:

```bash
cargo run -p baml-agent-runner
```

## Grafana dashboard

A pre-provisioned dashboard is included:

- **Agent Platform / Agent Platform Overview**

It contains:
- A2A request latency
- LLM calls/sec by function
- LLM duration by function
- Prompt payload bytes by function
- Tokens in/out by function
- Tool call rate + latency
- ONNX inference wait/run (avg ms) by operation
- ONNX wait-to-run ratio by operation
- ONNX wait-dominant events/sec by operation

## Notes

- Prometheus metric names are normalized to underscore style (`.` -> `_`).
- For per-agent filtering, use labels from `OTEL_RESOURCE_ATTRIBUTES` (for example `agent_package`/`service_instance_id` labels depending on exporter normalization).
- If you run many concurrent agents, create Grafana dashboard variables for `service_name`, `service_instance_id`, and `agent_package`.
