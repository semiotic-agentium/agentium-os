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
- `jq` (also used by `install.sh` to map `.env` secrets into Helm values)
- `curl`
- Docker or compatible image builder
- `git-lfs` (fastembed ONNX models tracked via Git LFS; runner image build fails on pointer stubs)
- Rust toolchain for local demo image builds
- TypeScript 6.x for regenerating/checking demo agents
- Agentium runner image containing `baml-agent-runner`, `baml-agent-builder`, and `cargo-agent-platform`
- Grafana MCP adapter available inside the runner/deployer image. `Dockerfile.demo` preinstalls `/usr/local/bin/mcp-grafana`, matching Helm defaults.
- Grafana MCP config for local agent deployment (`~/.agentium-os/mcp-servers.json` by default)

Local agent deployment against a port-forwarded runner:

```bash
cargo run -q -p cargo-agent-platform -- mcp enable grafana \
  --config ~/.agentium-os/mcp-servers.json \
  --repository-url http://127.0.0.1:18080/repository \
  --yes

BAML_MCP_REGISTRY_URL=http://127.0.0.1:18080/repository \
  cargo run -q -p cargo-agent-platform -- regen \
    --path demo/ford-observability/agents/observability-coordinator \
    --path demo/ford-observability/agents/grafana-investigator \
    --path demo/ford-observability/agents/slack-notify

cargo run -q -p cargo-agent-platform -- push \
  --repository-url http://127.0.0.1:18080/repository \
  --url http://127.0.0.1:18080 \
  --agents \
    demo/ford-observability/agents/observability-coordinator \
    demo/ford-observability/agents/grafana-investigator \
    demo/ford-observability/agents/slack-notify
```

Cluster shortcut after agent/BAML changes:

```bash
just ford-demo-reload k3d agentium
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

Runner image must include `cargo-agent-platform`, `support/grafana-alerts`, `support/slack_notify`, and the Grafana MCP adapter runtime. Use root `Dockerfile.demo` for this demo; it intentionally omits read-only Slack `support/slack` to avoid the Slack inbox producer consuming Grafana webhook ingress. Or push to registry and set `images.registry`, `images.tag`, and `agentiumRunner.image.*` values.

### Fastembed models (Git LFS)

The runner image bakes `models/fastembed/**/*.onnx` (embedding + reranker). These are Git LFS files. Without LFS materialization, the blobs are 134-byte pointer stubs and the Dockerfile stub-check aborts the build:

```text
ERROR: ONNX models are LFS pointer stubs (run 'git lfs pull'):
/models/fastembed/.../model.onnx
```

One-time host setup:

```bash
sudo apt install -y git-lfs   # or: brew install git-lfs
git lfs install
git lfs pull
```

Verify (real model is ~150MB, stub is 134B):

```bash
wc -c models/fastembed/models--jinaai--jina-reranker-v1-turbo-en/blobs/c1296c66c119de645fa9cdee536d8637740efe85224cfa270281e50f213aa565
```

Then `docker build -t agentium-runner:demo -f Dockerfile.demo .` succeeds.

Local `cargo run` may appear to work without LFS pull — runner falls back to `~/.cache/fastembed/` populated by previous fastembed downloads. Docker image has no such fallback cache, so LFS pull is mandatory for image builds.

## Secrets

Full E2E demo requires repo-root `.env`. `install.sh` / `just ford-demo-*` automatically source it and map known vars into Helm values.

```bash
OPENROUTER_API_KEY=...
GRAFANA_PASSWORD=admin
SLACK_BOT_TOKEN=xoxb-...
SLACK_NOTIFY_CHANNEL_ID=C0123456789
```

Notes:

- `OPENROUTER_API_KEY` is required for agent LLM calls.
- `GRAFANA_PASSWORD` is required for Grafana UI and MCP access with default basic auth.
- `SLACK_BOT_TOKEN` and `SLACK_NOTIFY_CHANNEL_ID` are required for Slack notification path.
- `SLACK_NOTIFY_CHANNEL_ID` must be channel ID, not name.

If overriding MCP config to token auth, provide:

```bash
GRAFANA_API_KEY=...      # service account token with read access to datasources/annotations
```

Manual `export` also works, but `.env` is recommended for demo operators.

No manual export needed if values are in `.env`:

```bash
demo/ford-observability/demo.sh install
```

Override env file path or disable loading:

```bash
ENV_FILE=/path/to/demo.env demo/ford-observability/demo.sh install
LOAD_ENV_FILE=0 demo/ford-observability/demo.sh install
AUTO_VALUES_FROM_ENV=0 demo/ford-observability/demo.sh install
```

You can still pass secrets directly; CLI `--set` overrides env-derived values:

```bash
demo/ford-observability/demo.sh install \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.grafanaAdminPassword="$GRAFANA_PASSWORD" \
  --set secrets.slackBotToken="$SLACK_BOT_TOKEN" \
  --set secrets.slackNotifyChannelId="$SLACK_NOTIFY_CHANNEL_ID"
```

Or use `existingSecrets.name` with keys documented in `helm/values.yaml`.

## Install

```bash
demo/ford-observability/demo.sh install
```

Equivalent Helm command:

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.grafanaAdminPassword="$GRAFANA_PASSWORD"
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
kubectl -n agentium-demo port-forward svc/grafana 3000:3000 &
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080 &
kubectl -n agentium-demo port-forward svc/prometheus 9090:9090 &  # optional
kubectl -n agentium-demo port-forward svc/loki 3100:3100 &        # optional
```

URLs:

- Grafana: <http://127.0.0.1:3000> (`admin` / value `secrets.grafanaAdminPassword`, default `admin`)
- HighLatency alert rule: <http://127.0.0.1:3000/alerting/grafana/high-latency/view?tab=query>
- Agentium dashboard: `http://127.0.0.1:18080/?view=dashboard&contextId=<context_id>`
- Raw transcript: `http://127.0.0.1:18080/contexts/<context_id>/conversation-history`
- LLM provenance: `http://127.0.0.1:18080/provenance/llm-calls?context_id=<context_id>`
- Tool provenance: `http://127.0.0.1:18080/provenance/tool-calls?context_id=<context_id>`

## Demo flow

### 1. Fresh install

```bash
just ford-demo-nuke k3d agentium
```

This deletes `agentium-demo`, rebuilds images, loads them into k3d cluster `agentium`, installs chart, and reads repo-root `.env` secrets.

### 2. Verify baseline

```bash
kubectl -n agentium-demo get pods
kubectl -n agentium-demo get deploy
```

Open Grafana and Slack. Check service health dashboard. k6 should drive ~50 RPS to `checkout-api`.

Keep port-forwards running:

```bash
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080 &
kubectl -n agentium-demo port-forward svc/grafana 3000:3000 &
```

Open alert query view:

```txt
http://127.0.0.1:3000/alerting/grafana/high-latency/view?tab=query
```

Wait at least 10 minutes before injecting. Agents compare incoming alert data against recent baseline/history; if injected after only ~1 minute, there may not be enough prior data to detect and explain latency spike reliably.

### 3. Inject latency spike

```bash
just ford-demo-inject
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

### 4. Watch alert + investigation

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

Watch runner logs during investigation:

```bash
kubectl -n agentium-demo logs -f agentium-runner-0 -c runner
```

### 5. Reset

```bash
demo/ford-observability/demo.sh reset
```

Keep ledger:

```bash
KEEP_LEDGER=1 demo/ford-observability/demo.sh reset
```

### 6. Smoke e2e

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

## MCP adapter dependency

Agentium's MCP runtime does not include Grafana-specific tools by itself. It starts the configured MCP adapter command from `mcp-servers.json` / Helm values and speaks MCP over stdio.

Default Helm values use the pre-baked Grafana MCP binary from `Dockerfile.demo`:

```yaml
agentiumRunner:
  mcp:
    grafana:
      command: /usr/local/bin/mcp-grafana
      args: ["-t", "stdio"]
      env:
        GRAFANA_URL: http://grafana:3000
        GRAFANA_USERNAME: admin
        HOME: /tmp
        XDG_CACHE_HOME: /tmp/.cache
      secretEnvName: GRAFANA_PASSWORD
```

This avoids runtime PyPI/network egress for `uvx mcp-grafana` during demos.

Grafana itself is still installed by the Helm chart as the `grafana` StatefulSet. The MCP adapter only connects to it; it does not install or configure Grafana.

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
kubectl -n agentium-demo logs statefulset/failure-harness
kubectl -n agentium-demo exec statefulset/failure-harness -- curl -fsS localhost:8080/admin/ledger
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
