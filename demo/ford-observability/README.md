# Ford Observability Demo

Agentium observability incident copilot demo. Runs checkout/payments dummy services, k6 load, Prometheus, Loki, Grafana, failure harness, Agentium runner, Grafana alert ingestion, investigator agents, and Slack notification.

Canonical entrypoint: `demo/ford-observability/demo.sh`.

## Honest framing

- Metrics are real Prometheus metrics from live demo services.
- Logs are real structured application logs from pod stdout, shipped to Loki.
- Trace/span evidence is synthetic span-like JSON mirrored into Grafana annotations by the failure harness. No Tempo/OTLP trace backend in this demo.
- Final incident report lives in Agentium provenance. Grafana annotations contain evidence/timeline only, not report body.
- Slack post is pointer + summary only. Provenance remains canonical record.

## Prerequisites

- Kubernetes cluster. k3d is default target; kind works if images are loaded/pushed.
- `kubectl`
- `helm`
- `jq`
- `curl`
- Docker or compatible image builder
- Rust toolchain for local demo image builds
- TypeScript 6.x for regenerating/checking demo agents
- Agentium runner image containing demo tools/agents, or local image loaded into cluster

For local agent regeneration with MCP tools:

```bash
BAML_MCP_REGISTRY_URL=http://127.0.0.1:18080/repository \
  cargo run -q -p cargo-agent-platform -- regen \
    --path demo/ford-observability/agents/observability-coordinator \
    --path demo/ford-observability/agents/grafana-investigator \
    --path demo/ford-observability/agents/slack-notify
```

Demo agent `tsconfig.json` files use `moduleResolution: "bundler"`.

## Images

Chart defaults use:

```yaml
images:
  registry: ghcr.io/semiotic-ai/agent-platform-demo
  tag: latest
  pullPolicy: IfNotPresent
```

Build/push or build/load images before install. Example for k3d local loop:

```bash
# Build demo service images from demo/ford-observability workspace, then import into k3d.
# Exact image names must match helm values overrides.
k3d image import ghcr.io/semiotic-ai/agent-platform-demo/checkout-api:latest -c <cluster>
k3d image import ghcr.io/semiotic-ai/agent-platform-demo/payments-api:latest -c <cluster>
k3d image import ghcr.io/semiotic-ai/agent-platform-demo/failure-harness:latest -c <cluster>
k3d image import agentium-runner:demo -c <cluster>
```

Or push to registry and set `images.registry`, `images.tag`, and `agentiumRunner.image.*` values.

## Secrets

Minimum:

```bash
export OPENROUTER_API_KEY=...
```

Slack optional but needed for notification path:

```bash
export SLACK_BOT_TOKEN=xoxb-...
export SLACK_NOTIFY_CHANNEL_ID=C0123456789   # channel ID, not name
```

Install can pass secrets directly:

```bash
demo/ford-observability/demo.sh install \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.slackBotToken="$SLACK_BOT_TOKEN" \
  --set secrets.slackNotifyChannelId="$SLACK_NOTIFY_CHANNEL_ID"
```

Or use `existingSecrets.name` with keys documented in `helm/values.yaml`.

## Install

```bash
demo/ford-observability/demo.sh install \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
```

Equivalent Helm command:

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
```

Useful env knobs:

```bash
NAMESPACE=agentium-demo
RELEASE=agentium-observability-demo
VALUES_FILE=/path/to/values.local.yaml
WAIT_ROLLOUTS=1
ROLLOUT_TIMEOUT=5m
```

## Port-forwards

```bash
kubectl -n agentium-demo port-forward svc/grafana 3000:3000
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080
kubectl -n agentium-demo port-forward svc/prometheus 9090:9090   # optional
kubectl -n agentium-demo port-forward svc/loki 3100:3100         # optional
```

URLs:

- Grafana: <http://127.0.0.1:3000> (`admin` / value `secrets.grafanaAdminPassword`, default `admin`)
- Agentium dashboard: `http://127.0.0.1:18080/?view=dashboard&contextId=<context_id>`
- Raw transcript: `http://127.0.0.1:18080/contexts/<context_id>/conversation-history`
- LLM provenance: `http://127.0.0.1:18080/provenance/llm-calls?context_id=<context_id>`
- Tool provenance: `http://127.0.0.1:18080/provenance/tool-calls?context_id=<context_id>`

## Demo flow

### 1. Verify baseline

```bash
kubectl -n agentium-demo get pods
kubectl -n agentium-demo get deploy
```

Open Grafana. Check service health dashboard. k6 should drive ~50 RPS to `checkout-api`.

### 2. Inject latency spike

```bash
demo/ford-observability/demo.sh inject
```

Tunable:

```bash
INCIDENT_ID=demo-latency-001 \
DURATION_SECONDS=300 \
LATENCY_MS_P95=1800 \
ERROR_RATE=0.02 \
demo/ford-observability/demo.sh inject
```

Harness writes ledger row, activates checkout failure mode, writes Grafana incident/trace annotations, then stops after duration.

### 3. Watch alert + investigation

Grafana `HighLatency` alert fires from Prometheus metrics and posts webhook to Agentium runner `/webhooks/grafana`.

Agentium flow:

```txt
grafana.alert.v1 event -> observability-coordinator -> grafana-investigator
  -> Prometheus metrics + Loki logs + synthetic trace annotations
  -> coordinator final report -> slack-notify -> support/slack_notify
```

Find context via e2e output, runner contexts API, or logs. Open:

```txt
http://127.0.0.1:18080/?view=dashboard&contextId=<context_id>
```

Presenter note: keep Grafana open for telemetry/evidence timeline. Open Agentium dashboard for investigation/report. Grafana does not contain report body.

### 4. Reset

```bash
demo/ford-observability/demo.sh reset
```

Keep ledger:

```bash
KEEP_LEDGER=1 demo/ford-observability/demo.sh reset
```

### 5. Smoke e2e

```bash
demo/ford-observability/demo.sh e2e
```

Skips install if desired:

```bash
SKIP_INSTALL=1 demo/ford-observability/demo.sh e2e
```

Outputs artifacts under `demo/ford-observability/.e2e-out/<incident_id>/`:

- `context_id.txt`
- `conversation-history.json`
- `llm-calls.json`
- `tool-calls.json`
- `ledger.json`

## Expected report shape

```txt
🚨 Grafana Alert: HighLatency firing
Service: checkout-api
Severity: warning

Summary:
- p95 latency rose from baseline to ~1.8s during alert window.
- Error rate stayed low; degradation, not outage.
- Service stayed up.
- Loki logs show dependency timeout warnings for payments-api.
- Synthetic trace annotation points to slow POST /payments/authorize span.

Evidence:
- Prometheus p95 latency query
- Request/error/up queries
- Loki log samples
- Grafana annotation trace sample

Links:
- Grafana dashboard/panel
- Agentium dashboard/provenance URL

Suggested next actions:
1. Check payments-api dependency latency.
2. Confirm expected demo injection.
3. Watch alert resolution.
```

## Failure modes

Must-have mode:

- `latency_spike` via `demo.sh inject`

Deferred modes may have placeholder scripts/values but are not required for first recording:

- `dependency_timeout`
- `brief_offline`
- scheduled injection mode

## Troubleshooting

### No pods or rollout stuck

```bash
kubectl -n agentium-demo get pods
kubectl -n agentium-demo describe pod <pod>
kubectl -n agentium-demo logs deploy/<name>
```

Likely causes: image not loaded/pushed, wrong image tag, missing secret, PVC/storage class issue.

### No alert

Check:

```bash
kubectl -n agentium-demo logs deploy/k6-load-generator
kubectl -n agentium-demo port-forward svc/prometheus 9090:9090
```

PromQL:

```promql
histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service="checkout-api"}[2m])) by (le))
```

Need k6 traffic and active failure mode. Grafana evaluation may lag scrape by up to ~1 minute.

### No Agentium context

Check runner and webhook path:

```bash
kubectl -n agentium-demo logs deploy/agentium-runner
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080
curl -fsS http://127.0.0.1:18080/readyz
```

Check Grafana contact point points to `http://agentium-runner:18080/webhooks/grafana`.

### No MCP data

Verify Grafana datasource provisioning and runner MCP registry. Investigator needs:

- `mcp/grafana/list_datasources`
- `mcp/grafana/query_prometheus`
- `mcp/grafana/query_loki_logs`
- `mcp/grafana/get_annotations`

### Missing Loki logs

Check Alloy:

```bash
kubectl -n agentium-demo logs deploy/alloy
```

In Grafana Explore, query:

```logql
{service="checkout-api"} |= "payments-api"
```

Labels may also appear as Kubernetes labels depending on shipper config.

### Missing trace annotations

Harness is sole annotation writer. Check:

```bash
kubectl -n agentium-demo logs deploy/failure-harness
kubectl -n agentium-demo exec deploy/failure-harness -- curl -fsS localhost:8080/admin/ledger
```

Grafana API token/admin config must permit `POST /api/annotations`.

### Low RPS / weak p95 shift

Check k6 and values:

```bash
kubectl -n agentium-demo logs deploy/k6-load-generator
```

Default RPS is 50. Increase `k6.rps`, `k6.preAllocatedVUs`, or `k6.maxVUs` if cluster can handle more.

## Files

```txt
demo/ford-observability/
  demo.sh                 # install|inject|reset|e2e
  scripts/                # operator commands
  helm/                   # deployable chart
  services/               # Rust demo binaries
  agents/                 # coordinator, investigator, slack notifier
  k6/load.js              # load generator script
```

Related docs:

- `docs/demos/ford-grafana/runbook.md`
- `docs/demos/ford-grafana/demo-script.md`
- `docs/demos/ford-grafana/demo-recording.md`
