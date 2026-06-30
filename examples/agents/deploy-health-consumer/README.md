<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

# deploy-health-consumer

The consumer end of the raw external-datasource demo. It subscribes to the
`deploy-health` source produced by
[`examples/external-tools/deploy-health-datasource`](../../external-tools/deploy-health-datasource)
and runs `onDispatch` on each event, closing the full chain:

```
POST webhook → runner enqueue → producer drain → dispatch → THIS agent.onDispatch
```

## What it does

- **Subscribes** (`manifest.json > discovery.subscriptions`) to
  `schema_versions: ["deploy-health.v1"]`, `source_kinds: ["deploy-health"]` —
  matching the `routing_key` (= source_kind) and `message_type` (= schema_version)
  the `RawDatasourceProducer` stamps on every event.
- **`onDispatch(ctx)`** reads `ctx.request.messages[0]` (the raw webhook body),
  builds a one-line deploy-health incident summary, and returns
  `{ accepted: true, detail }`.
- **No tools, no inter-agent calls** (`manifest.json > tools: []`). The summary is
  deterministic and in-process, so the demo runs without API keys or any other
  deployed agent. A realistic agent would open a triage task or call a tool here.

## Build

```bash
agentium build --path examples/agents/deploy-health-consumer
```

## Run the full two-part demo

1. Start a runner with the datasource activated **and** a repository (so the agent
   can be deployed into it):

   ```bash
   cargo run -p agentium -- serve -- \
     --serve-http 127.0.0.1:18087 \
     --runner-config examples/external-tools/deploy-health-datasource/runner.toml \
     --state-dir ./.runner-state-demo \
     --repository-dir ./.repository-demo \
     --repository-url http://127.0.0.1:18087/repository \
     --event-poll-interval-secs 1
   ```

2. Publish + deploy this agent into that runner:

   ```bash
   ./examples/agents/deploy-health-consumer/deploy.sh http://127.0.0.1:18087
   ```

3. POST a deploy-health event:

   ```bash
   curl -sS -i -X POST \
     http://127.0.0.1:18087/webhooks/ext/examples/deploy-health-datasource/events \
     -H 'content-type: application/json' \
     -d '{"service":"checkout","environment":"prod","status":"degraded","deploy_id":"d-456"}'
   ```

## Expected outcome

HTTP `202`, then in the runner logs:

```
A2aAgent::handle_dispatch envelope routing=deploy-health …
event delivery complete producer_key=external-datasource:examples/deploy-health-datasource:events matched=1 accepted=1
```

`matched=1` = this agent's subscription matched the produced event; `accepted=1` =
its `onDispatch` accepted it. (Without a subscriber the dispatcher instead logs
`no subscribed agents matched produced event; advancing checkpoint`.)
