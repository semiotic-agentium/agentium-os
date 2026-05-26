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
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
```

With Slack notification:

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY" \
  --set secrets.slackBotToken="$SLACK_BOT_TOKEN" \
  --set secrets.slackNotifyChannelId="$SLACK_NOTIFY_CHANNEL_ID"
```

`SLACK_NOTIFY_CHANNEL_ID` should be `C...` channel ID, not display name.

Demo wrapper:

```bash
demo/ford-observability/demo.sh install --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
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
  slackBotToken: ""
  slackNotifyChannelId: ""

agentiumRunner:
  image:
    repository: agentium-runner
    tag: demo
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
- Failure harness writes Grafana annotations and SQLite ledger.
- Agentium runner receives Grafana alerts at `/webhooks/grafana`.
- Final report is in Agentium provenance, not Grafana annotations.
