# Task Daemon Leadership Demo Runbook

This runbook is optimized for a high-reliability leadership demo with this flow:

1. Poll real Slack channel discussion (`#agentium-eng`)
2. Interpret discussion with LLM (project-aware)
3. Handoff typed interpretation payload to coordinator over A2A
4. Show provenance timeline (`/contexts/{id}/metrics`)
5. Show provenance mermaid sequence export

## Demo Outcome

By the end, audience should see that project discussion can be converted into structured, auditable orchestration input with traceability.

## Required Environment

In `.env`:

- `SLACK_BOT_TOKEN` (or `SLACK_USER_TOKEN`)
- `SLACK_WORKSPACE_URL`
- `OPENROUTER_API_KEY` (primary)
- Optional local fallback:
  - `TASK_DAEMON_LLM_FALLBACK_BASE_URL=http://localhost:1234/v1`
  - `TASK_DAEMON_LLM_FALLBACK_MODEL=<model-name>`

## Slack App Setup (From Scratch)

1. Create a new Slack app from manifest.
   - Use [docs/slack-app-manifest.example.yaml](/Users/joseph/git/semiotic-agentium/agent-platform/docs/slack-app-manifest.example.yaml)
2. Confirm OAuth redirect URL matches your helper config.
   - Default: `https://localhost:8787/slack/oauth/callback`
3. Install app to workspace and capture:
   - `SLACK_APP_CLIENT_ID`
   - `SLACK_APP_CLIENT_SECRET`
4. Generate install URL:

```bash
SLACK_APP_CLIENT_ID="..." \
SLACK_REDIRECT_URI="https://localhost:8787/slack/oauth/callback" \
./scripts/slack-oauth-helper.sh install-url
```

5. Open URL, authorize, copy `code` from callback URL.
6. Exchange code for tokens:

```bash
SLACK_APP_CLIENT_ID="..." \
SLACK_APP_CLIENT_SECRET="..." \
SLACK_REDIRECT_URI="https://localhost:8787/slack/oauth/callback" \
./scripts/slack-oauth-helper.sh exchange-code --code "<oauth-code>"
```

7. Add printed `export` values into your `.env` (do not commit secrets).

## Pre-Demo Smoke Checks

Slack read check (no coordinator dependency):

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --once \
  --extractor heuristic \
  --state-file /tmp/task-daemon-smoke-state.json \
  --jsonl-out /tmp/task-daemon-smoke.jsonl \
  --no_stdout
```

LLM check (real interpretation path):

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --once \
  --extractor llm \
  --state-file /tmp/task-daemon-llm-state.json \
  --jsonl-out /tmp/task-daemon-llm.jsonl \
  --no_stdout
```

## One-Command Demo Flow

```bash
just task-daemon-demo
```

This does:

1. Starts coordinator runner with file-backed provenance
2. Runs task-daemon once with `--a2a-live --emit-empty`
3. Captures `context_id` from coordinator response logs
4. Fetches timeline metrics from `GET /contexts/{context_id}/metrics`
5. Exports mermaid sequence to `/tmp/task-daemon-demo-sequence.mmd`

## Demo Artifacts

Default outputs:

- Task-daemon JSONL: `/tmp/task-daemon-demo-batch.jsonl`
- Task-daemon log: `/tmp/task-daemon-demo.log`
- Metrics timeline JSON: `/tmp/task-daemon-demo-metrics.json`
- Mermaid diagram: `/tmp/task-daemon-demo-sequence.mmd`

## Stop / Cleanup

```bash
just task-daemon-demo-stop
```

## Useful Overrides

- `TASK_DAEMON_DEMO_CHANNEL` (default `agentium-eng`)
- `TASK_DAEMON_DEMO_COORDINATOR_PORT` (default `8082`)
- `TASK_DAEMON_DEMO_COORDINATOR_URL` (default `http://127.0.0.1:<port>`)
- `TASK_DAEMON_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `TASK_DAEMON_DEMO_EXTRACTOR` (`llm` default)
- `TASK_DAEMON_DEMO_START_COORDINATOR` (`1` default)

## Failure Plan (Tomorrow)

1. If OpenRouter is degraded, use local fallback provider (`TASK_DAEMON_LLM_FALLBACK_*`).
2. If Slack auth fails, rerun OAuth exchange and verify channel access (`#agentium-eng`).
3. If coordinator boot fails, run `just coordinator-demo-stop` then retry `just task-daemon-demo`.
4. If context capture fails, inspect `/tmp/task-daemon-demo.log` and `/tmp/coordinator-runner.log`.
