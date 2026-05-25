# Demo: Agentium Observability Incident Copilot

Demonstrate Agentium-OS as an agentic observability layer that reacts to real Grafana alerts from a live Kubernetes workload, investigates evidence through Grafana MCP, summarizes likely cause and impact, posts a human-facing summary to Slack, and exposes the full investigation in provenance by `context_id`.

Full design: [`demo_plan.md`](./demo_plan.md). This issue tracks the work split.

## Architecture

```txt
           ┌────────────────────┐
           │   checkout-api +    │   ─── real Prometheus metrics ──┐
           │   payments-api      │                                  │
           │   (Rust services)   │── structured stdout logs ─┐      │
           └─────────┬──────────┘                             │      │
                     │ continuous load                         ▼      │
                     │                                              ▼
                ┌────┴────┐                                  ┌────────────┐
                │   k6    │                                  │ Prometheus │
                │ 50 RPS  │                                  └─────┬──────┘
                └─────────┘                              ┌──────────────┐ │
                                                         │ Loki + Alloy │ │
                                                         └──────┬───────┘ │
                                                                ▼         ▼
                ┌──────────────────┐                          ┌──────────┐
                │ Failure harness  │── annotations + ledger ─►│ Grafana  │
                │ (sole annotator) │── /admin/failure-mode    └─────┬────┘
                └──────────────────┘                                │
                                                                    │ webhook
                                                                    ▼
        ┌──────────────────────────────────────────────────────────────┐
        │ Agentium runner                                              │
        │ ┌────────────────────────────┐  ┌──────────────────────────┐ │
        │ │ grafana-alerts tool        │  │ Agents                    │ │
        │ │  ├ webhook route           │  │  ├ coordinator            │ │
        │ │  ├ fingerprint→context_id  │  │  ├ grafana-investigator   │ │
        │ │  ├ IngressStore enqueue    │  │  └ slack-notify           │ │
        │ │  └ ProducedEvent emitter   │  │     (calls support/       │ │
        │ └─────────────┬──────────────┘  │      slack_notify)        │ │
        │               │ ProducedEvent    └──────────┬───────────────┘ │
        │               ▼                              │ A2A             │
        │   ┌─────────────────────────┐               │                 │
        │   │ host subscription match │──onDispatch──►│                 │
        │   └─────────────────────────┘               │                 │
        └────────────────────────────────────────────┬─┴───────────────┘
                                                     │
                                                     ▼
                                            ┌────────────────┐
                                            │ Slack channel  │
                                            └────────────────┘

  Investigation context_id is the global investigation identity.
  All evidence + final report land in Agentium provenance under that context_id.
```

## Scope (first recording)

- **One failure mode**: `latency_spike` (checkout-api degraded by slow payments-api).
- **One alert**: Grafana `HighLatency`.
- **One investigator**: collapsed metrics + Loki logs + synthetic span evidence into single grafana-investigator.
- **One outbound surface**: Slack post via new `support/slack_notify` write tool.
- **Local Kubernetes**: k3d (matches `just e2e-k8s` harness).
- **Honest framing**: logs are real Loki-backed structured application logs. Span/trace "evidence" is synthetic structured records mirrored into Grafana annotations. Not Tempo.
- **Outbound is per-tool by design**. Host event substrate is inbound-only; no host-backed notification sink planned.

## Components

| Component | Path | Role |
|---|---|---|
| `checkout-api` | `demo/ford-observability/services/checkout-api/` | Rust dummy service; emits metrics, JSON stdout logs for Loki, and synthetic span records on injection. |
| `payments-api` | `demo/ford-observability/services/payments-api/` | Rust dummy downstream. |
| `failure-harness` | `demo/ford-observability/services/failure-harness/` | Rust daemon. Owns ledger, on-demand injection, and **all** Grafana annotation writes. |
| k6 load gen | `demo/ford-observability/k6/` | Constant 50 RPS baseline so histograms move within alert window. |
| Loki + log shipper | `demo/ford-observability/helm/` | Real log backend. Grafana datasource plus Alloy/promtail shipping pod stdout logs to Loki. |
| `grafana-alerts` tool | `crates/tools/grafana-alerts/` | Webhook route + fingerprint↔`context_id` mapping + `EventProducer` emitting `grafana.alert.v1`. Modeled on `crates/tools/slack`. |
| `slack_notify` tool | `crates/tools/slack-notify/` | One write op (`chat.postMessage`). Strict input `{ text, context_id }` with `deny_unknown_fields`. Channel from `SLACK_NOTIFY_CHANNEL_ID` (must be `C…` ID; name fallback resolved once at startup). |
| `coordinator` agent | `agents/coordinator/` (new) | `onDispatch(grafana.alert.v1)` → delegates to grafana-investigator → synthesizes → hands summary to slack-notify via A2A. |
| `grafana-investigator` agent | `agents/grafana-investigator/` (new) | Grafana MCP: `query_prometheus`, Loki/LogQL datasource query, `get_annotations` for trace/window records (bounded `limit` + time-window). Returns structured findings. |
| `slack-notify` agent | `agents/slack-notify/` (new) | Calls `support/slack_notify`. Read-only `agents/slack-agent/` is **not** reused. |
| Helm chart | `demo/ford-observability/helm/` | Single `helm upgrade --install` for whole stack. |
| Runbook + script | `docs/demos/ford-grafana/` | Install, inject, reset, troubleshoot, expected output, recording script. |

## Key contracts

- **Investigation identity**: Agentium `context_id`. Grafana `fingerprint`/`groupKey` maps to it via the `grafana-alerts` tool's small SQLite table. Firing reuses active mapping; resolved appends to same context; new firing after resolved mints a fresh context.
- **Event shape**: `ProducedEvent` (schema_version=`grafana.alert.v1`, source_kind=`grafana`, source_key=`grafana:local`, `context_id` + `message_id`). Host subscription matching builds the dispatch.
- **Coordinator subscription**:
  ```json
  {
    "schema_versions": ["grafana.alert.v1"],
    "source_kinds": ["grafana"],
    "source_keys": ["grafana:local"]
  }
  ```
- **Coordinator cap**: ~60s wall clock per investigation; emit progress on each tool call so dashboard streams render.
- **Single annotation writer**: failure-harness only. `grafana-alerts` tool never touches Grafana write APIs.
- **Slack thread per `context_id`**: derived inside `support/slack_notify`. LLM has no `channel` or `thread_key` control.

## Retrieval paths (fallback order)

1. Slack channel post (primary human surface).
2. Raw provenance API: `/contexts/{id}/conversation-history`, `/provenance/llm-calls`, `/provenance/tool-calls`.
3. Web dashboard `${AGENTIUM_UI_BASE_URL}/?view=dashboard&contextId={id}` (verify route in target branch before recording).
4. Web chat UI (`web/`) attached to `context_id` — deferred final step.

## Out of scope for first recording

Tempo / Mimir, MS Teams, generic webhook delivery, Slack-as-input via existing `agents/slack-agent/`, scheduled injection mode, additional failure modes, ledger-assertion runner (ledger **writes** still required), real FireHydrant / PagerDuty / OnCall, multi-cluster hardening, automated remediation, host-backed notification sink (not planned).

---

## Task checklist

Each top-level item is a candidate sub-issue. Order roughly matches dependency order.

### Demo services
- [ ] `demo/ford-observability/` workspace skeleton (Cargo workspace outside main workspace, `rust-toolchain.toml`, README stub).
- [ ] `checkout-api` Rust service: routes, Prometheus metrics, admin endpoints, JSON stdout logs for Loki, synthetic span emission on injection.
- [ ] `payments-api` Rust service: routes, metrics, slowable `POST /payments/authorize`.
- [ ] k6 load script + ConfigMap (constant 50 RPS).

### Failure harness
- [ ] `failure-harness` Rust daemon skeleton.
- [ ] On-demand `POST /admin/failure-mode` + `/stop` + `/reset-active`.
- [ ] Ground-truth ledger schema + SQLite write on injection start/end.
- [ ] Ledger read API: `GET /admin/ledger`, `GET /admin/ledger/{incident_id}`, `POST /admin/reset-ledger`.
- [ ] Grafana annotation writer (`kind=window`, `kind=trace`, `kind=window,status=resolved`) — sole writer of annotations. Logs live in Loki, not annotations.

### Grafana + Prometheus + Loki config
- [ ] Prometheus scrape config ConfigMap.
- [ ] Loki deployment/service and log shipper (Alloy preferred; promtail acceptable).
- [ ] Grafana Loki datasource provisioning.
- [ ] Grafana dashboard JSON ConfigMap.
- [ ] Grafana `HighLatency` alert rule ConfigMap.
- [ ] Grafana webhook configured to target runner `/webhooks/grafana`.

### `grafana-alerts` tool crate (in-runner)
- [ ] Crate skeleton `crates/tools/grafana-alerts/`, modeled on `crates/tools/slack`.
- [ ] Tool metadata declares `event_sources = ["grafana"]`; inventory registration of `EventProducerProvider`.
- [ ] Webhook route served by runner (`POST /webhooks/grafana`).
- [ ] SQLite mapping table: fingerprint/groupKey → `context_id` with firing / resolved / re-firing semantics.
- [ ] `IngressStore::enqueue` on webhook receipt; payload schema.
- [ ] `GrafanaAlertEventProducer::poll`: drain `IngressStore`, emit `ProducedEvent` with `context_id` + `message_id`.
- [ ] Tests covering reuse / resolved / re-firing.

### `support/slack_notify` write tool
- [ ] Crate `crates/tools/slack-notify/` (or write surface in `crates/tools/slack`).
- [ ] Strict input: `{ text, context_id }` + `serde(deny_unknown_fields)`.
- [ ] `SLACK_NOTIFY_CHANNEL_ID` resolution: accept `C…` directly; resolve `#name` once at startup via `conversations.list` or fail to start.
- [ ] `chat.postMessage` integration; threading derived from `context_id` (in-memory map).
- [ ] Tool registration in tool registry; provenance tool-call archive verified.
- [ ] Tests: input rejection (extra fields, channel override), startup name resolution.

### Agents
- [ ] `agents/coordinator/`: `onDispatch(grafana.alert.v1)`, parse alert, delegate to grafana-investigator via A2A, synthesize report, hand summary to slack-notify via A2A. ~60s cap, progress messages.
- [ ] `coordinator` manifest subscription (`schema_versions`, `source_kinds`, `source_keys`).
- [ ] `agents/grafana-investigator/`: `mcp/grafana/list_datasources`, `query_prometheus`, Loki/LogQL query via Grafana, `get_annotations` for trace/window records (with `limit` + time bound). Return structured findings (metrics, Loki logs, synthetic spans) with archive refs. Log queries filter by `service` + time window, not `incident_id`.
- [ ] `agents/slack-notify/`: receive summary via A2A, format message, call `support/slack_notify`.

### Helm + deployment
- [ ] Chart skeleton `demo/ford-observability/helm/` (Chart.yaml, values.yaml, README).
- [ ] Templates: checkout-api, payments-api, k6, failure-harness, prometheus, loki, alloy/promtail, grafana, agentium-runner (also serves `grafana-alerts` webhook), dashboards/alerts/datasource ConfigMaps, secrets template.
- [ ] Just recipes: `demo-observability-images`, `demo-observability-install`, `demo-observability-inject`, `demo-observability-reset`, `demo-observability-e2e` (smoke path).
- [ ] Verify k3d local install end-to-end.

### Runbook + recording
- [ ] `docs/demos/ford-grafana/runbook.md`: install, inject, reset, port-forward, expected output, honest framing disclaimer, troubleshooting.
- [ ] `docs/demos/ford-grafana/demo-script.md`: scripted narration including the long-lived / context-reuse callout.
- [ ] Verify dashboard route in target branch; fall back to Slack + raw provenance API if broken.
- [ ] Record demo video.

### Nice-to-have (after first recording)
- [ ] Ledger-assertion runner (E2E eval) and `just demo-observability-e2e` assertion stage.
- [ ] Web chat UI (`web/`) attachment to `context_id`.
- [ ] Additional failure modes (`dependency_timeout`, `brief_offline`).
- [ ] Resolved-alert summary update beyond minimal provenance message.
- [ ] Scheduled injection mode.
- [ ] Webhook signing / HMAC on `grafana-alerts` route.

## Deliverables

1. Recorded video walking through one `latency_spike` end-to-end.
2. Helm-installable demo (`just demo-observability-install`) reproducible on k3d.
3. Slack post + dashboard + raw provenance retrieval paths working.
