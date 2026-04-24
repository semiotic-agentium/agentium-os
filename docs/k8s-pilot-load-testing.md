# Kubernetes pilot load testing

Repeatable load-test harness for the supported Kubernetes pilot package.
This document is the in-repo benchmark contract for issue `#226`:
canonical scenarios, default workload parameters, generated-output schema,
and operator instructions.

Commit the benchmark contract, not the benchmark exhaust.

The repo keeps the harness code, scenario definitions, defaults, and output
schema in git. Per-run artifacts under `artifacts/load-test/<timestamp>/`
are generated artifacts and are intentionally not committed. The repo does
not try to store the authoritative raw run history or a checked-in numbers
document for every rerun.

If you need a baseline run for `#226`, record it externally, preferably in
an Issue #226 comment or the closing PR comment, with the generated
artifacts attached or linked.

## Benchmark contract

- Canonical scenarios: `local_a2a`, `forwarded_a2a`, `split_dual_runner`
- Default workload contract:
  - warmup `30s`
  - measured duration `120s`
  - concurrency sweep `1,8,32`
  - fixed payload `dispatch-echo load probe`
- Supported target: deterministic `dispatch-echo/default` over
  `POST /agents/{package}/{instance}/a2a`
- Supported bringup path:
  `scripts/verify-k8s-pilot-package.sh --keep-cluster --values deploy/helm/agentium-os/examples/k3d-load-test-values.yaml`
- Output location: `artifacts/load-test/<timestamp>/`
- Output contract: `topology.json`, `<scenario>.json`, `summary.json`,
  `summary.md`
- Raw outputs are generated artifacts, not committed product docs
- A baseline run is only meaningful when its external record includes the
  git SHA, run date, scenario set, defaults or overrides used, environment
  and topology notes, and an artifact attachment or equivalent evidence
  location

## Prerequisites

Host tools: Docker, k3d, kubectl, helm, jq, curl, Rust toolchain (for the
agent builder binary the harness calls during publish), and Node 22 (the
load runner is plain `.mjs` with no external dependencies).

The harness reuses the supported install path. It does **not** invent a
separate Kubernetes topology:

- Bringup is delegated to `scripts/verify-k8s-pilot-package.sh` with a
  Helm values overlay that enables OTLP export
  (`deploy/helm/agentium-os/examples/k3d-load-test-values.yaml`).
- OTEL traces and metrics flow to the local observability stack in
  [`observability/`](../observability/) via the k3d host alias
  `host.k3d.internal`. The harness verifies that alias is reachable from
  inside a runner pod before starting load.

## One-command run

```
just k8s-load-test
```

This runs, in order:

1. Preflights the host toolchain and builds `baml-agent-builder` if missing.
2. Starts `observability/docker-compose.yml` (OTel Collector on `4317`,
   Prometheus on `9090`, Grafana on `3000`).
3. Runs `scripts/verify-k8s-pilot-package.sh --keep-cluster --values
   deploy/helm/agentium-os/examples/k3d-load-test-values.yaml`.
4. Reads the real `runner-token` from the cluster secret (the lib.sh
   random default doesn't match the installed secret across subshells).
5. Probes OTLP reachability from inside the runner pod using
   `kubectl exec … node -e 'net.createConnection(...)'` (the runtime
   image has Node 22 but no bash, so `/dev/tcp` is unavailable).
6. Opens direct-pod port-forwards to `runner-0` on `localhost:18081`
   and `runner-1` on `localhost:18082`, with a pre-bind check that
   refuses to run if another process already answers on those ports
   (matches `scripts/k8s-pilot-smoke.sh` safety pattern).
7. For each scenario: publishes or deploys `dispatch-echo`, records
   Prometheus counters, runs `scripts/load-test/run.mjs`, reads the
   post-run counter deltas, and always tears the scenario back down
   before returning success or failure.
8. Writes generated `summary.json` and `summary.md` under the artifacts
   directory and prints the markdown summary to stdout.

Total expected runtime on a laptop: ~5 min first-time image build +
~2 min bringup + 3 × (warmup 30s + measured 120s) × 3 concurrency
levels ≈ **~28 min**.

## Flags

### `scripts/k8s-load-test.sh`

| Flag | Default | Notes |
|------|---------|-------|
| `--scenarios <csv>` | `local_a2a,forwarded_a2a,split_dual_runner` | Subset or full set. |
| `--concurrency <csv>` | `1,8,32` | Concurrency levels swept per scenario. |
| `--warmup-seconds <n>` | `30` | Stats dropped during warmup. |
| `--measured-seconds <n>` | `120` | Measured window. |
| `--payload <text>` | `dispatch-echo load probe` | Fixed request payload. |
| `--package <name>` | `dispatch-echo` | Agent to target. |
| `--instance <id>` | `default` | Instance id. |
| `--skip-bringup` | off | Use an already-running cluster. The port-forward pre-bind check still protects against collisions. |
| `--skip-observability` | off | Do not run `docker compose up -d` on `observability/docker-compose.yml`. An observability stack exposing OTLP gRPC on host `:4317` and Prometheus on host `:9090` must already be reachable — see *Flag caveat: `--skip-observability`* below. |
| `--skip-builder-build` | off | Fail if `baml-agent-builder` is missing. |
| `--artifacts-dir <path>` | `artifacts/load-test/<timestamp>` | Output directory. |
| `--verify-arg <arg>` | — | Repeatable; passthrough to `verify-k8s-pilot-package.sh` (for example `--verify-arg --no-build`). |

#### Flag caveat: `--skip-observability`

`--skip-observability` only short-circuits `start_observability`, which
is the step that runs `docker compose up -d` on
`observability/docker-compose.yml`. It does **not** skip:

- `probe_otlp_reachability` — the wrapper still `kubectl exec`s into
  `runner-0` and opens a TCP connection to each candidate host alias
  (`host.k3d.internal` / `host.docker.internal` /
  `host.containers.internal`) on port `4317`. If no alias answers, the
  run aborts before any scenario starts.
- Per-scenario Prometheus counter queries (`prom_scalar_sum`,
  `prom_serving_breakdown`). If Prometheus on `http://localhost:9090`
  isn't serving the runner's metrics, the `serving_delta` comes back
  empty and the scenario-shape assertion hard-fails. The 30-second
  `wait_for_prom_scrape_since` poll also fires between load-end and the
  post-snapshot regardless of the flag.

Pass `--skip-observability` only when an observability stack exposing
OTLP gRPC on host `:4317` and Prometheus on host `:9090` is already up
by some other means (for example a shared compose stack from a prior
session). Otherwise bringup, the OTLP probe, or the scenario-shape gate
will fail loudly rather than silently produce empty deltas.

### `scripts/load-test/run.mjs`

Pure load generator. Can be invoked directly against an already-prepared
cluster if you want to iterate on parameters without re-running bringup.

| Flag | Default | Notes |
|------|---------|-------|
| `--scenario` | — (required) | `local_a2a` / `forwarded_a2a` / `split_dual_runner`. |
| `--ingress <url>[,<url>]` | — (required) | One URL for local / forwarded; two for split. |
| `--artifacts-dir <path>` | — (required) | Where to write `<scenario>.json`. |
| `--package` | `dispatch-echo` | |
| `--instance` | `default` | |
| `--concurrency` | `1,8,32` | |
| `--warmup-seconds` | `30` | |
| `--measured-seconds` | `120` | |
| `--payload` | `dispatch-echo load probe` | |
| `--expect-substring` | `dispatch-echo does not handle A2A messages` | Must appear in the response body. |
| `--topology-json` | — | Optional `topology.json` path to embed in output. |
| `--request-timeout-ms` | `30000` | Per-request timeout. |

The runner exits non-zero only if no requests completed at all (a broken
ingress or setup). A high error rate at any concurrency level is recorded
in the scenario JSON (`errorBreakdown`, `errorSamples`) and surfaced as
a warning; it does not abort the sweep, because the point of the load test
is to measure how the pilot behaves under load, including the concurrency
levels where it queues or times out. When that non-zero exit happens
under the wrapper, `run_scenario` still collects post-run Prometheus
deltas when it already took the pre-snapshot, then tears the fixture down
before returning the Node runner's exit status.

The load-bearing correctness gate is the wrapper's scenario-shape
assertion on the Prometheus serving delta (see below). That check is
pod-identity specific (matches `$RUNNER_POD_0` / `$RUNNER_POD_1`) and
hard-fails the whole run if violated, so you cannot accidentally claim a
valid baseline where the wrong pod served traffic.

## Generated artifacts

Each run writes to `artifacts/load-test/<timestamp>/`:

- `topology.json` — Helm release + image repo/tag, both runner DNS
  endpoints, the overlay values file used, OTLP endpoint, Prometheus URL,
  git SHA, and the OTLP probe result.
- `<scenario>.json` — per-scenario machine-readable record (fields below).
- `summary.json` — aggregate: artifacts dir + per-scenario observability
  deltas.
- `summary.md` — human-readable per-run summary generated from the same
  run output.

These files are generated artifacts. They are intentionally not committed.
If you want to preserve a run for review or issue closure, attach the run
directory externally or link to an external copy from an Issue #226 or PR
comment. The repo contains the benchmark contract, not the authoritative
raw run history.

`summary.md` is convenience output for that specific run. It is not meant
to become a version-controlled product document, and the repo does not
use checked-in markdown summaries as durable benchmark provenance.

### Per-scenario JSON

```json
{
  "scenario": "local_a2a",
  "start_ts": "...",
  "end_ts": "...",
  "warmup_seconds": 30,
  "measured_seconds": 120,
  "concurrency_levels": [
    {
      "concurrency": 1,
      "total": "<int>",
      "success": "<int>",
      "error": "<int>",
      "throughputRps": "<float>",
      "timeToResponseHeadersMs": { "count": "<int>", "min": "<float>", "max": "<float>", "mean": "<float>", "p50": "<float>", "p90": "<float>", "p95": "<float>", "p99": "<float>" },
      "timeToResponseCompleteMs": { "count": "<int>", "min": "<float>", "max": "<float>", "mean": "<float>", "p50": "<float>", "p90": "<float>", "p95": "<float>", "p99": "<float>" },
      "errorBreakdown": { "http_error": "<int>", "payload_mismatch": "<int>", "network_error": "<int>", "timeout": "<int>" }
    }
  ],
  "ingress_targets": ["http://localhost:18081/agents/dispatch-echo/default/a2a"],
  "package": "dispatch-echo",
  "instance": "default",
  "expect_substring": "dispatch-echo does not handle A2A messages",
  "payload_bytes": "<int>",
  "topology": { "<topology.json contents>": "..." },
  "observability": {
    "prometheus_url": "http://localhost:9090",
    "baml_rt_cluster_a2a_forward_total_delta": "<float>",
    "baml_rt_a2a_request_total_delta": "<float>",
    "baml_rt_a2a_request_total_by_serving_delta": { "<service.instance.id>": "<requests_this_scenario>" },
    "baml_rt_a2a_request_total_by_serving": { "<service.instance.id>": "<cumulative>" }
  }
}
```

If a scenario fails the wrapper's shape assertion, the per-scenario JSON
is still written so the failing run's raw numbers and
`observability.baml_rt_a2a_request_total_by_serving_delta` remain
available on disk for inspection. `summary.json` and `summary.md` are
only produced when the full sweep passes.

## Recording a baseline externally

For `#226`, a baseline run should be recorded outside git. Preferred
location: an Issue #226 comment or the closing PR comment.

The external record should include:

- exact git SHA
- run date, and time if available
- exact command line
- scenario set that ran
- default workload contract or any overrides used
- environment and topology notes, including whether this was local k3d,
  local observability, and the local image-distribution exception
- artifact attachment or equivalent evidence location
- any material caveats, failures, or deviations from the default contract

A baseline run is only meaningful if those provenance fields and the
artifact evidence stay together. Do not leave the only authoritative git
SHA or run context inside a local ignored directory with no external
record.

## Truthful baseline claims

A truthful baseline claim for `#226` should say:

- the repo now contains the supported load harness and benchmark contract
- the actual baseline run evidence lives outside git
- the claim identifies the exact git SHA and run parameters
- the claim states whether the full default contract ran or which
  overrides were used
- the claim links or attaches the generated artifacts
- the repo is not pretending to version every run's results as source
  documentation

That is the intended closure model: the repo is the contract; the issue
thread or PR thread is the evidence log for actual runs.

## Interpreting results

### Scenario-specific expectations

Assertions run against the **per-scenario delta** (post-pre snapshot of
the serving breakdown), not the cumulative counter, and they match
specific pod identities, not just "how many runners appeared." Each
check hard-fails the run (no `Load-test PASSED` line, exit non-zero).

The pre-snapshot is taken **after** the wrapper's fail-fast smoke
request (which is sent by the wrapper, not the Node runner, precisely
so it can snapshot afterwards with a Prometheus scrape-catchup wait).
That means `serving_delta` reflects only the measured warmup+load
traffic from `run.mjs`: smoke contributions are excluded, so they cannot
on their own satisfy a multi-runner shape check.

At `c=1` the single worker still alternates ingress targets per request
(round-robin through the full `--ingress` list), so `split_dual_runner`
with `--concurrency 1` genuinely divides traffic across both URLs rather
than pinning to the first.

- `local_a2a` — deploy on runner-0 only, all traffic to runner-0.
  Required: `forward_delta ≈ 0`; `serving_delta` keys exactly equal
  `[<runner-0 pod name>]`.
- `forwarded_a2a` — deploy on runner-1 only, metadata published to
  runner-0, all traffic to runner-0. Required: `forward_delta > 0`
  (grows by roughly the success count); `serving_delta` keys exactly
  equal `[<runner-1 pod name>]`, meaning the peer served the traffic.
- `split_dual_runner` — deploy on both runners, traffic split 50/50
  across explicit per-pod ingress URLs. Required: both runner-0 and
  runner-1 pod names present in `serving_delta` with positive counts.

A shape-assertion mismatch hard-fails the scenario: `assert_scenario_shape`
logs a `[FAIL]` line and returns non-zero, `run_scenario` propagates that
return code, and the wrapper aborts before reaching `write_summary` or
the `Load-test PASSED` line.

Cleanup is finally-style once scenario setup starts: `run_scenario`
captures the scenario failure explicitly, calls `teardown_scenario`
before returning, and preserves the original benchmark failure if cleanup
also reports trouble. If cleanup is the only thing that fails, cleanup
becomes the scenario failure. If `run.mjs` failed first, the wrapper skips
the shape assertion rather than replacing the Node failure with a second,
less specific error.

### What the timings mean

Client-side timings, captured by the Node runner:

- `timeToResponseHeadersMs` — time from issuing the `fetch()` to the
  response headers being available (TTFB).
- `timeToResponseCompleteMs` — headers + full body read. This is the
  primary latency number to compare across scenarios.

The `/a2a` endpoint returns a JSON array of collected JSON-RPC responses,
not a stream. There is no "time to first SSE data line" field by design.

## Running a single scenario

```
just k8s-load-test --scenarios forwarded_a2a
```

Or, against an already-running cluster (skip the image build and chart
install):

```
just k8s-load-test --skip-bringup --scenarios local_a2a --concurrency 8
```

To iterate on just the load generator without any orchestration, run the
Node runner directly:

```
node scripts/load-test/run.mjs \
  --scenario local_a2a \
  --ingress http://localhost:18081 \
  --artifacts-dir /tmp/loadtest-ad-hoc \
  --concurrency 4 --warmup-seconds 5 --measured-seconds 15
```

## Troubleshooting

- **"localhost:18081 already responds to /healthz"** — another process is
  bound there (stale port-forward, concurrent `just e2e-k8s`, local
  runner). Stop it and retry. The check is intentional: without it, a
  run could silently benchmark the wrong cluster and produce misleading
  evidence.
- **"In-pod OTLP TCP probe failed"** — the observability stack isn't
  reachable via `host.k3d.internal:4317` from inside the runner pod.
  Check `docker compose -f observability/docker-compose.yml ps`. The
  Prometheus deltas in the summary will still be usable if OTLP comes up
  after the probe (the collector scrapes runner metrics directly), but
  traces won't populate.
- **`runner-token` secret missing** — bringup didn't finish cleanly; rerun
  `scripts/verify-k8s-pilot-package.sh --keep-cluster --values
  deploy/helm/agentium-os/examples/k3d-load-test-values.yaml` manually
  and inspect the output.
- **Error rate > 1% at some concurrency level** — `scripts/load-test/run.mjs`
  prints `[load] warn: error rate N.NN%` to stdout and continues the
  sweep; high error rates at higher concurrency are data, not a harness
  abort (the runner only exits non-zero when *zero* requests completed,
  indicating a broken ingress or setup). Inspect the per-scenario JSON
  `errorBreakdown` and `errorSamples` for failure detail. The wrapper's
  pass/fail gate is the scenario-shape assertion on the Prometheus
  serving delta, not error rate.
