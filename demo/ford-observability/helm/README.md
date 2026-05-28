# Agentium Observability Demo Helm Chart

Deploys Ford observability demo stack into Kubernetes:

- checkout-api
- payments-api
- k6 load generator
- failure-harness
- Prometheus
- Loki
- Alloy log shipper
- Grafana dashboards/alerts/datasources
- Agentium runner with Grafana webhook route and demo agents

## Install

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.grafanaAdminPassword="$GRAFANA_PASSWORD"
```

With Slack notification:

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.grafanaAdminPassword="$GRAFANA_PASSWORD" \
  --set secrets.slackBotToken="$SLACK_BOT_TOKEN" \
  --set secrets.slackNotifyChannelId="$SLACK_NOTIFY_CHANNEL_ID"
```

`SLACK_NOTIFY_CHANNEL_ID` should be `C...` channel ID, not display name.

Demo wrapper loads repo-root `.env` by default and maps known secret env vars into Helm values:

```bash
demo/ford-observability/demo.sh install
```

Known env vars: `OPENROUTER_API_KEY`, `GRAFANA_PASSWORD`, `GRAFANA_API_KEY`/`GRAFANA_API_TOKEN` (only for token-auth overrides), `SLACK_BOT_TOKEN`, `SLACK_USER_TOKEN`, `SLACK_NOTIFY_CHANNEL_ID`, `RUNNER_TOKEN`.

Override:

```bash
ENV_FILE=/path/to/demo.env demo/ford-observability/demo.sh install
LOAD_ENV_FILE=0 demo/ford-observability/demo.sh install
```

## Common values

```yaml
images:
  registry: ghcr.io/semiotic-ai/agent-platform-demo
  tag: latest
  pullPolicy: IfNotPresent

secrets:
  openrouterApiKey: ""
  grafanaAdminPassword: admin
  grafanaApiToken: ""
  grafanaApiKey: ""
  slackBotToken: ""
  slackNotifyChannelId: ""

agentiumRunner:
  image:
    repository: agentium-runner
    tag: demo
  mcp:
    grafana:
      enabled: true
      serverId: grafana
      command: /usr/local/bin/mcp-grafana
      args: ["-t", "stdio"]
      env:
        GRAFANA_URL: http://grafana:3000
        GRAFANA_USERNAME: admin
        HOME: /tmp
        XDG_CACHE_HOME: /tmp/.cache
      secretEnvName: GRAFANA_PASSWORD
  uiBaseUrl: http://localhost:18080
  webhookPath: /webhooks/grafana
  model: x-ai/grok-4.3
```

Use `existingSecrets.name` to mount externally managed secrets. Expected key names live in `values.yaml`.

## Ports

```bash
kubectl -n agentium-demo port-forward svc/grafana 3000:3000
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080
```

- Grafana: <http://127.0.0.1:3000>
- Agentium dashboard: `http://127.0.0.1:18080/?view=dashboard&contextId=<context_id>`

## Notes

- Chart assumes images already exist in cluster registry or have been loaded into local cluster.
- Runner/deployer image must contain `cargo-agent-platform`; root `Dockerfile.demo` copies it into `/usr/local/bin`.
- Runner/deployer image should be built with demo-only features: `baml-agent-runner/grafana-alerts,baml-agent-runner/slack-notify,baml-rt-builder/slack-notify`. Avoid full `http-tools` for this demo until ingress namespacing lands.
- Runner/deployer image must contain Grafana MCP adapter runtime. `Dockerfile.demo` bakes `/usr/local/bin/mcp-grafana`; values default to that path.
- Agent deployer Helm hook waits for runner, enables Grafana MCP, regenerates demo agents, then `push`es all three agents.
- Failure harness writes Grafana annotations and SQLite ledger.
- Agentium runner receives Grafana alerts at `/webhooks/grafana`.
- Final report is in Agentium provenance, not Grafana annotations.
