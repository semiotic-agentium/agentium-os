<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
SPDX-FileCopyrightText: 2026 Karakara Labs

SPDX-License-Identifier: Apache-2.0
-->

# Raw external datasource demo (`deploy-health`)

A **raw** external datasource: the runner parses the webhook body in-process,
turns it into `messages[0]`, and drains it through a generic producer. It is
**manifest-only**: `tool-manifest.json` declares the `datasources[]` entry and
the `events[]` contract inline, and no tool process ever runs.

This is the shape for any provider that pushes alerts to a webhook (Grafana
alerting, deploy pipelines, ...): the event lands on the runner, routes to every
agent subscribed to its `source_kind`, and can start a full A2A workflow
(investigate, delegate, notify).

## How it loads

1. `runner.toml [external_tools].dirs` lists this directory. At boot the runner
   reads the manifest and validates it: at least one `datasources[]` entry, a
   matching `events[]` schema for each raw datasource's `schema_version`, and
   no `runtime` (datasources never run a process).
2. `runner.toml [external_datasources."examples/deploy-health-datasource"."events"]`
   **activates** it. A listed dir is necessary but not sufficient: without this
   entry the datasource stays dormant. Delete it and the route disappears on the
   next boot.

Webhook routes are mounted at startup, so a new or changed datasource takes
effect on the next boot.

## Run it

```bash
./examples/external-tools/deploy-health-datasource/run_runner.sh
# equivalently, from the repo root:
#   cargo run -p karakara-os -- serve -- \
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
