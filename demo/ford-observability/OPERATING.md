# Ford Observability Demo — Operating Cheatsheet

Concise day-to-day operator commands. See `README.md` for full reference, architecture, and rationale.

Namespace: `agentium-demo`. Release: `agentium-observability-demo`.

## Prereqs

- `kubectl`, `helm`, `jq`, `curl`, `docker`, `k3d` (or `kind`)
- `git lfs pull` once (fastembed ONNX models — build fails on pointer stubs)
- `.env` at repo root with at least `OPENROUTER_API_KEY`. Optional: `GRAFANA_PASSWORD`, `SLACK_BOT_TOKEN`, `SLACK_NOTIFY_CHANNEL_ID`, `RUNNER_TOKEN`

## Install / Upgrade

```bash
# build images + load into k3d + helm install
just ford-demo-setup k3d agentium

# install only (images already loaded)
just ford-demo-install

# rebuild/load/reinstall demo stack after agent or image changes
just ford-demo-reload k3d agentium
```

Env knobs for `install.sh`:
```bash
HELM_TIMEOUT=25m     # post-upgrade hook (agent-deployer Job) can be slow
ROLLOUT_TIMEOUT=5m   # kubectl rollout status per workload
WAIT_ROLLOUTS=0      # skip rollout waits
AUTO_VALUES_FROM_ENV=0   # don't synthesize Helm values from .env
```

## Wipe state (digest mismatch / corrupt registry)

```bash
helm -n agentium-demo uninstall agentium-observability-demo
kubectl -n agentium-demo delete pvc data-agentium-runner-0
just ford-demo-setup k3d agentium
```

Nuclear: `kubectl delete ns agentium-demo && just ford-demo-setup k3d agentium`.

## Port-forwards

```bash
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080 &
kubectl -n agentium-demo port-forward svc/grafana 3000:3000 &
kubectl -n agentium-demo port-forward svc/prometheus 9090:9090 &   # optional
kubectl -n agentium-demo port-forward svc/loki 3100:3100 &         # optional
```

URLs:
- Agentium UI: <http://127.0.0.1:18080>
- Agentium dashboard for a context: `http://127.0.0.1:18080/?view=dashboard&contextId=<id>`
- Grafana: <http://127.0.0.1:3000> (`admin` / `$GRAFANA_PASSWORD`)

## Health checks

```bash
curl -s http://127.0.0.1:18080/readyz
curl -s http://127.0.0.1:18080/healthz
curl -s http://127.0.0.1:18080/agents | jq
curl -s http://127.0.0.1:18080/cluster/agents | jq   # cluster runner placement
```

## Drive the demo

```bash
just ford-demo-inject       # trigger latency_spike failure mode
just ford-demo-reset        # clear failure mode + ledger
just ford-demo-e2e          # inject + wait for coordinator + dump artifacts
```

Tunables for inject:
```bash
INCIDENT_ID=demo-latency-001 DURATION_SECONDS=300 \
LATENCY_MS_P95=1800 ERROR_RATE=0.02 \
  just ford-demo-inject
```

E2E artifacts: `demo/ford-observability/.e2e-out/<incident_id>/`
- `context_id.txt`, `conversation-history.json`, `llm-calls.json`, `tool-calls.json`, `ledger.json`

## Chat with agents

```bash
# via SDK CLI
cargo run -p cargo-agent-platform -- chat \
  --agent observability-coordinator \
  --url http://127.0.0.1:18080

# raw JSON-RPC A2A (SSE)
curl -N -X POST http://127.0.0.1:18080/agents/observability-coordinator/default/chat \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send",
       "params":{"message":{"role":"user","parts":[{"text":"Investigate the current latency spike"}]}}}'
```

## Logs

```bash
# runner (StatefulSet)
kubectl -n agentium-demo logs -f agentium-runner-0 -c runner --tail=200

# agent-deployer hook Job (after install — failed pod sticks for inspection)
kubectl -n agentium-demo get pods -l job-name=agentium-demo-agent-deployer
kubectl -n agentium-demo logs -l job-name=agentium-demo-agent-deployer --all-containers --tail=-1

# demo services
kubectl -n agentium-demo logs -f deploy/checkout-api
kubectl -n agentium-demo logs -f deploy/payments-api
kubectl -n agentium-demo logs -f statefulset/failure-harness
kubectl -n agentium-demo logs -f deploy/k6-load-generator
kubectl -n agentium-demo logs -f daemonset/alloy

# events sorted by recency
kubectl -n agentium-demo get events --sort-by=.lastTimestamp | tail -30
```

Bump runner verbosity: edit `helm/values.yaml` → `agentiumRunner.rustLog: "debug,baml_rt=trace,baml_rt_mcp=debug"` then `just ford-demo-install`.

## Provenance API

```bash
curl -s http://127.0.0.1:18080/contexts | jq
curl -s "http://127.0.0.1:18080/contexts/<id>/conversation-history" | jq
curl -s "http://127.0.0.1:18080/provenance/llm-calls?context_id=<id>" | jq
curl -s "http://127.0.0.1:18080/provenance/tool-calls?context_id=<id>" | jq
curl -s http://127.0.0.1:18080/tasks/<task-id>/episode | jq
```

## Inspect demo state inside cluster

```bash
# k6 stats
kubectl -n agentium-demo exec deploy/k6-load-generator -- sh -c 'true'

# failure harness ledger
kubectl -n agentium-demo exec statefulset/failure-harness -- \
  curl -fsS localhost:8080/admin/ledger | jq

# MCP server configured into runner
kubectl -n agentium-demo exec agentium-runner-0 -- \
  cat /config/mcp-servers.json
```

## Quick PromQL (port-forward 9090 first)

```promql
# p95 checkout latency
histogram_quantile(0.95,
  sum(rate(demo_service_request_duration_seconds_bucket{service="checkout-api"}[2m])) by (le))

# error rate
sum(rate(demo_service_requests_total{service="checkout-api",status=~"5.."}[2m]))
  / sum(rate(demo_service_requests_total{service="checkout-api"}[2m]))
```

## Quick LogQL

```logql
{service="checkout-api"} |= "payments-api"
{service="payments-api"} |~ "timeout|error"
```

## Investigator Loki checklist (after inject)

Capture runner logs during an alert investigation, then verify Loki was queried and synthesis did not fabricate log lines:

```bash
kubectl -n agentium-demo logs -f agentium-runner-0 -c runner --since=1s | tee demo_ford.logs
# in another terminal: just ford-demo-inject

rg 'query_loki_logs|mcp/grafana/query_loki' demo_ford.logs
rg 'FSM step: (Open|Send).*loki' demo_ford.logs   # optional if trace logging enabled
rg 'log_samples' demo_ford.logs
```

Success:
- At least one `mcp/grafana/query_loki_logs` tool session (Open → Send → Finish) per investigation.
- `log_samples[].line` values match raw Loki JSON log lines (structured `service=`, `route=`, `failure_mode=` fields from checkout-api), not generic English placeholders.

Failure (pre-fix pattern):
- Only `query_prometheus` in tool dispatch; `log_samples` contain invented sentences like `"High latency observed"`.

After agent/BAML changes: `just ford-demo-reload k3d agentium`, then re-run inject + grep.

## Iteration loops

| Change | Command |
|---|---|
| Agent TS / BAML | `just ford-demo-reload k3d agentium` |
| Helm values only | `just ford-demo-install` |
| Runner Rust | `just ford-demo-reload k3d agentium` |
| Full reset | wipe-state block above |

## Debug failed hook Job

Job uses `restartPolicy: Never`, `backoffLimit: 0`, `hook-delete-policy: before-hook-creation`. On failure, pod stays for inspection until next install.

```bash
POD=$(kubectl -n agentium-demo get pods -l job-name=agentium-demo-agent-deployer -o name | head -1)
kubectl -n agentium-demo logs "$POD" -c unpack-demo-agents
kubectl -n agentium-demo logs "$POD" -c wait-for-runner
kubectl -n agentium-demo logs "$POD" -c push-agents
kubectl -n agentium-demo describe pod "$POD" | tail -40
```

## Common failure modes

| Error | Fix |
|---|---|
| `post-upgrade hooks failed ... context deadline exceeded` | bump `HELM_TIMEOUT=40m` |
| `MCP servers config required but unreadable` | runner missing `BAML_MCP_SERVERS_CONFIG=/config/mcp-servers.json` env — check `templates/agentium-runner.yaml` |
| `Tool metadata missing for: support/grafana-alerts` or `support/slack_notify` | runner image built without demo features — rebuild `just ford-demo-build-images` (`Dockerfile.demo`) |
| `no subscribed agents matched ... schema=host.source-records.v1 source_kind=slack source_key=grafana:local` | runner image includes read-only Slack `support/slack`; rebuild demo image from `Dockerfile.demo`, not full `http-tools` |
| `launch config digest mismatch` | stale registry on PVC; wipe runner PVC (see Wipe state) |
| `Temporary failure in name resolution` (uvx PyPI) | replaced by pre-baked `/usr/local/bin/mcp-grafana` in image; rebuild image |
| `ONNX models are LFS pointer stubs` | `git lfs pull` |
