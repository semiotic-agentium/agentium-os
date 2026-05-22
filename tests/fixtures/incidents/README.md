# Incident Fixtures

Seeded outage datasets used by the Ford/Grafana investigation slice
([#512](https://github.com/semiotic-agentium/agentium-os/issues/512)).
Each scenario directory pairs LGTM-style evidence (metrics, logs, traces, annotations)
with sample incident events shaped so a real producer (Grafana Alerting,
FireHydrant, Slack/operator console) can later replace them without
changing the downstream agent or provenance contract.

## Scenarios

| Scenario ID | Affected service | Symptom | Owns |
| --- | --- | --- | --- |
| [`lgtm-checkout-5xx`](./lgtm-checkout-5xx/README.md) | `checkout-api` | 5xx surge after deploy v2.34.0 | First Ford/Grafana dataset (#521 slice 1) |

## What lives in a scenario

```
<scenario_id>/
  README.md                # operator-facing runbook for the scenario
  scenario.json            # manifest: ids, services, time range, file map
  ground-truth.json        # narrative cause and timeline; NOT for the agent prompt
  expected-evidence.json   # machine-readable evidence references for #513 rehearsal
  metrics/                 # Prometheus/Mimir range-query-shaped JSON
  logs/                    # Loki-stream-shaped JSON
  traces/                  # Tempo trace/span-shaped JSON
  annotations/             # Grafana dashboard annotations / deploy markers
  events/                  # sample incident event payloads (Grafana, FireHydrant, manual)
```

Files under `metrics/`, `logs/`, `traces/`, `annotations/` are shaped like
the responses a real Grafana datasource would return. They are not yet
served by a live LGTM stack — they're inert JSON the agent or rehearsal
can read directly while the live datasource is mocked or absent.

Files under `events/` are shaped like the webhooks a real producer
would emit. `fixture_only: true` is the explicit signal that this is a
sample, not a real-integration artifact.

## Loading from Rust

```rust
use test_support::incident_fixtures::{LoadedScenario, validate_scenario};

let scenario = LoadedScenario::load("lgtm-checkout-5xx").expect("load");
validate_scenario(&scenario).expect("integrity");

for path in scenario.evidence_files() {
    // ...
}
```

`validate_scenario` checks the scenario timeline, evidence-window
containment, event `scenario_ref`, and the `expected-evidence.json`
cross-reference. Use it in any new test that depends on a scenario.

## Resetting

There is nothing to reset for these fixtures — they are inert files on
disk. Edit them, run tests, revert.

## Adding a new scenario

1. Create `<scenario_id>/` with the layout above.
2. Run `cargo test -p test-support --test incident_fixtures_<scenario_id>`
   to verify integrity.
3. Add a row to the table above and link the scenario README.
