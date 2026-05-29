# `lgtm-checkout-5xx` — checkout-api 5xx surge after v2.34.0 deploy

First Grafana/LGTM seeded outage dataset. Owned by issue
[#521](https://github.com/semiotic-agentium/agentium-os/issues/521) and
used by [#519](https://github.com/semiotic-agentium/agentium-os/issues/519),
[#515](https://github.com/semiotic-agentium/agentium-os/issues/515), and
[#513](https://github.com/semiotic-agentium/agentium-os/issues/513).

## What this scenario is

| | |
| --- | --- |
| Affected service | `checkout-api` in `production` namespace, `k8s-prod-us-east-2` |
| Window | `2026-05-20T14:00:00Z` — `2026-05-20T14:45:00Z` |
| Alert | `CheckoutHighErrorRate` fires `14:09:30Z`, resolves `14:43:10Z` |
| Severity | sev2 (fixture-declared) |
| Cause (ground truth) | Deploy of `checkout-api v2.34.0` shipped a 200ms client timeout against `payments-gateway`, whose p95 sits at ~480ms, so most checkout requests now exhaust the client deadline. |
| Remediation (ground truth) | Roll back to `v2.33.4` or raise the client timeout to >= 1500ms. |

The agent does **not** receive `ground-truth.json`. Reviewers and the
#513 rehearsal use it to evaluate whether the agent's report names the
right cause and recommendation.

## What is real LGTM vs fixture data

| Surface | This dataset | A real Grafana/LGTM environment |
| --- | --- | --- |
| Metrics payload shape | Prometheus HTTP API range-query response (`resultType: matrix`) — inert JSON | Mimir/Prometheus returns the same shape over HTTP |
| Logs payload shape | Loki query response (`resultType: streams`) — inert JSON | Loki returns the same shape |
| Traces payload shape | Tempo search + exemplar trace JSON — inert JSON | Tempo returns the same shape |
| Annotations | Grafana annotation object — inert JSON | Grafana annotation API returns the same shape |
| Alert event | Webhook payload as `grafana.alert.v1` envelope | Grafana Alerting webhook posts the same payload to the host |
| Incident event | `firehydrant.incident.v1` envelope, fixture-only | A real FireHydrant integration would deliver the same shape; not in scope here |
| Operator event | `incident.manual.v1` envelope from the Event Console | Hand-typed event from #520 / #526 Event Console |

For a real Grafana/LGTM stack, swap the JSON readers for live Grafana
MCP tool calls. Time ranges, label sets, dashboard panel IDs, and the
trace exemplar shape are stable across the swap.

## Evidence map

The agent should retrieve evidence from at least two telemetry families
and make at least two MCP tool calls (#515 acceptance). The seeded
evidence here is sized to support that:

| Evidence ID | Family | File | Why it matters |
| --- | --- | --- | --- |
| `metric.checkout-api.http_5xx_rate` | metrics | `metrics/checkout-api-5xx-rate.json` | 5xx rate climbs ~0.2 → ~14 rps starting 14:08, returns at 14:42 |
| `metric.checkout-api.http_request_duration_p95` | metrics | `metrics/checkout-api-latency-p95.json` | p95 jumps from ~0.28s to ~1.6s in the same window |
| `metric.checkout-api.dependency_latency_payments_gateway_p95` | metrics | `metrics/payments-gateway-dependency-latency-p95.json` | Dependency p95 ~0.48s, exceeds the 200ms client timeout introduced by v2.34.0 |
| `log.checkout-api.upstream_timeout_payments_gateway` | logs | `logs/checkout-api-errors.json` | Lines say `upstream timeout calling payments-gateway after 200ms` |
| `trace.checkout-api.payments_gateway_timeout_example` | traces | `traces/checkout-api-payments-gateway-timeout.json` | Exemplar trace shows client span at 200ms, status ERROR, `error.kind=timeout` |
| `annotation.checkout-api.deploy_v2_34_0` | annotations | `annotations/checkout-api-deploy-v2.34.0.json` | Deploy marker at 14:02:30Z with the change summary |
| `metric.checkout-api.pod_restarts` | metrics | `metrics/checkout-api-pod-restarts.json` | Red herring: a single restart spike at 14:05 that does NOT explain 35 minutes of 5xx |
| `metric.checkout-api.http_request_rate` | metrics | `metrics/checkout-api-request-rate.json` | Supporting: stable ~185 rps, rules out capacity loss |

`expected-evidence.json` is the machine-readable view of this table and
is what the rehearsal in #513 should assert against.

## Sample incident events

The trigger is evented. The host receives one of the events below,
matches a subscription, and dispatches the investigation. There is no
manual chat prompt in the demo path.

| File | `schema_version` | `source_kind` | Use it when |
| --- | --- | --- | --- |
| `events/grafana-alert-firing.json` | `grafana.alert.v1` | `grafana` | Primary path — Grafana Alerting fires `CheckoutHighErrorRate` |
| `events/grafana-alert-resolved.json` | `grafana.alert.v1` | `grafana` | Closing event after rollback; same fingerprint as firing |
| `events/firehydrant-incident-opened.json` | `firehydrant.incident.v1` | `firehydrant` | Fixture-only — exercises a realistic incident-tool input shape |
| `events/manual-operator-exploratory.json` | `incident.manual.v1` | `operator` | Operator-typed event from the Event Console (#520 / #526) |

Suggested investigation agent subscription (per Natanael's note on
[#521 comment](https://github.com/semiotic-agentium/agentium-os/issues/521)):

```json
{ "schema_versions": ["grafana.alert.v1"], "source_kinds": ["grafana"] }
```

Grafana webhook triggers the investigation. Grafana MCP then enriches
it — queries for the exact metrics/logs/traces the alert hints at, and
gathers the deploy annotation in the same window.

## How this plugs into the broader slice

- **#519** — sample incident event shapes here feed the event-trigger /
  report-sink contract. The same envelopes are reusable from the
  exploratory event console.
- **#515** — Grafana MCP fixture-driven agent path uses
  `expected-evidence.json` as the must-find list. At least two MCP
  tool calls across at least two telemetry families is satisfiable
  from the seeded files alone.
- **#513** — rehearsal asserts the agent retrieved evidence whose IDs
  overlap `expected-evidence.json[].evidence_id`, the report names a
  term from `expected_root_cause_terms_any_of`, and the recommendation
  names a term from `expected_recommendation_terms_any_of`.
- **#520 / #526** — Event Console can load these event files as
  draftable samples without inventing a bespoke console.

## Loading from Rust

```rust
use test_support::incident_fixtures::{LoadedScenario, validate_scenario};

let scenario = LoadedScenario::load("lgtm-checkout-5xx")?;
validate_scenario(&scenario)?;

let expected = scenario.expected_evidence()?;
// match the agent's retrieved evidence against expected["evidence"][].evidence_id
```

## Verifying the fixture is intact

```bash
just test-crate test-support
# or, narrower:
cargo nextest run -p test-support --test incident_fixtures_lgtm_checkout_5xx
```

This is the narrowest check. It does not require external services,
network, or credentials.
