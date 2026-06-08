<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

# Raw external datasource demo (`deploy-health`)

A **raw** external datasource: the runner parses the webhook body in-process,
turns it into `messages[0]`, and drains it through a generic producer. There is
**no per-webhook tool process** — the tool here is spawned only at boot for
discovery (`tool/describe` + `tool/schema`), which is how it advertises its
`events[]` contract.

## How it loads (same path as external tools)

1. `runner.toml [external_tools].dirs` lists this directory → at boot the runner
   discovers the manifest and **auto-approves** it (the dev/config trust path —
   no separate approval step is required for a configured dir). The snapshot is
   validated and cached under `--state-dir`.
2. `runner.toml [external_datasources."examples/deploy-health-datasource"."events"]`
   **activates** it. Approval is necessary but not sufficient — without this
   entry the datasource stays dormant. Delete it and the route disappears on the
   next boot.

To add a tool to an already-running runner instead, use the runner's
`POST /external-tools/enable` endpoint (or the `agent-platform` CLI). Note: a
newly enabled **datasource** only starts serving its webhook route on the next
boot, because webhook routes are mounted at startup.

## Run it

```bash
./examples/external-tools/deploy-health-datasource/run_runner.sh
# equivalently, from the repo root:
#   cargo run -p baml-agent-runner -- \
#     --runner-config examples/external-tools/deploy-health-datasource/runner.toml \
#     --serve-http 127.0.0.1:18080 \
#     --event-poll-interval-secs 1
```

## Send an event

The webhook path is runner-derived: `/webhooks/ext/<tool name>/<datasource key>`.

```bash
curl -sS -i -X POST \
  http://127.0.0.1:18080/webhooks/ext/examples/deploy-health-datasource/events \
  -H 'content-type: application/json' \
  -d '{"service":"checkout","environment":"prod","status":"degraded","deploy_id":"d-123"}'
```

Expected: **`202 Accepted`** (the configured success ack). The body becomes
`messages[0]` and is enqueued under `source_kind = "deploy-health"`,
`source_key = "deploy-health:local"`, then drained by the producer for any agent
subscribed to that source kind.

### Status matrix

| Request | Response |
|---|---|
| Valid JSON object (new or duplicate) | `202` — a duplicate is an idempotent hit, still success |
| Body is not a JSON object (array/scalar) | `400` |
| Invalid JSON | `400` |
| Enqueue / store failure | `5xx` — so the provider retries |

Idempotency: `ingress_id = sha256(source_kind ‖ 0x00 ‖ source_key ‖ 0x00 ‖ sha256(body))`,
so reposting the same body collapses to one event (set `dedupe_header` in the
manifest to dedupe on a provider idempotency header instead).
