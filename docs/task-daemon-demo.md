# Task Daemon Leadership Demo Runbook

This runbook is optimized for a high-reliability leadership demo with this flow:

1. Poll real Slack channel discussion (`#agentium-eng`)
2. Interpret discussion with LLM (project-aware)
3. Handoff typed interpretation payload to coordinator over A2A
4. Show provenance timeline (`/contexts/{id}/metrics`)
5. Show provenance sequence visuals (Mermaid + SVG)
6. Show a concise scoreboard for leadership

## Demo Outcome

By the end, audience should see that project discussion can be converted into structured, auditable orchestration input with traceability.

## Required Environment

In `.env`:

- `SLACK_BOT_TOKEN` (or `SLACK_USER_TOKEN`)
- `SLACK_WORKSPACE_URL`
- `OPENROUTER_API_KEY` (primary)
- `CLICKUP_API_KEY` (required when ClickUp specialist is enabled)
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
  --auth user \
  --once \
  --extractor heuristic \
  --state-file /tmp/task-daemon-smoke-state.json \
  --jsonl-out /tmp/task-daemon-smoke.jsonl \
  --no-stdout
```

LLM check (real interpretation path):

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --auth user \
  --once \
  --extractor llm \
  --state-file /tmp/task-daemon-llm-state.json \
  --jsonl-out /tmp/task-daemon-llm.jsonl \
  --no-stdout
```

## One-Command Demo Flow

```bash
just task-daemon-demo
```

This does:

1. Starts coordinator runner with file-backed provenance
   - Default specialist profile is `clickup` (`clickup-agent` on, `notion-agent` off)
2. Runs task-daemon once with `--a2a-live --emit-empty`
3. Captures `context_id` from task-daemon logs (fallback: coordinator log)
4. Fetches timeline metrics from `GET /contexts/{context_id}/metrics`
5. Exports sequence to Mermaid (`.mmd`) and Graphviz DOT (`.dot`)
6. Builds a stage DOT view (noise-reduced) for presentation
7. Renders SVG from stage DOT when Graphviz is installed
8. Writes a scoreboard markdown summary
9. Writes a stage-ready HTML demo report

The script resets prior state/artifacts by default (`TASK_DAEMON_DEMO_RESET_STATE=1`) so repeated runs still read channel history instead of reusing an old cursor.

## Demo Artifacts

Default outputs:

- Task-daemon JSONL: `/tmp/task-daemon-demo-batch.jsonl`
- Task-daemon log: `/tmp/task-daemon-demo.log`
- Metrics timeline JSON: `/tmp/task-daemon-demo-metrics.json`
- Mermaid sequence: `/tmp/task-daemon-demo-sequence.mmd`
- DOT graph (raw): `/tmp/task-daemon-demo-sequence.dot`
- DOT graph (stage): `/tmp/task-daemon-demo-sequence-stage.dot`
- SVG graph (stage): `/tmp/task-daemon-demo-sequence.svg` (if `dot` is installed)
- Scoreboard summary: `/tmp/task-daemon-demo-scoreboard.md`
- Stage report: `/tmp/task-daemon-demo-report.html`

## Visual Feedback Loop (Do Not Skip)

After `just task-daemon-demo`, review visuals in this order:

1. Open stage report (primary demo view):

```bash
open /tmp/task-daemon-demo-report.html
```

2. Open scoreboard and read the headline:

```bash
cat /tmp/task-daemon-demo-scoreboard.md
```

3. Open SVG graph for structure (agents, tools, call chain):

```bash
open /tmp/task-daemon-demo-sequence.svg
```

4. Open Mermaid sequence for timing narrative:

```bash
code /tmp/task-daemon-demo-sequence.mmd
```

5. Optional: if SVG is too dense, regenerate a focused graph for the same context:

```bash
cargo run -p baml-rt-provenance --features cli --bin graph_exporter -- \
  --db provenance.db \
  --context-id <context-id> \
  --simplify \
  --format dot \
  --output /tmp/task-daemon-demo-focused.dot
dot -Tsvg /tmp/task-daemon-demo-focused.dot -o /tmp/task-daemon-demo-focused.svg
```

## Stop / Cleanup

```bash
just task-daemon-demo-stop
```

## Useful Overrides

- `TASK_DAEMON_DEMO_CHANNEL` (default `agentium-eng`)
- `TASK_DAEMON_DEMO_AUTH` (`auto` default, use `user` for private channels)
- `TASK_DAEMON_DEMO_RESET_STATE` (`1` default; set `0` to reuse cursor/artifacts)
- `TASK_DAEMON_DEMO_COORDINATOR_PORT` (default `8082`)
- `TASK_DAEMON_DEMO_COORDINATOR_URL` (default `http://127.0.0.1:<port>`)
- `TASK_DAEMON_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `TASK_DAEMON_DEMO_EXTRACTOR` (`llm` default)
- `TASK_DAEMON_DEMO_MAX_CANDIDATES` (default `4`; lower values reduce coordinator latency)
- `TASK_DAEMON_DEMO_START_COORDINATOR` (`1` default)
- `TASK_DAEMON_DEMO_CONTEXT_ID` (optional explicit context id for post-run export)
- `TASK_DAEMON_DEMO_SKIP_POLL` (`0` default; set `1` to export visuals only)
- `TASK_DAEMON_DEMO_SPECIALIST_PROFILE` (default `clickup`; options: `clickup`, `notion`, `both`, `none`)
- `TASK_DAEMON_STAGE_DOT_EXCLUDE_PREFIXES` (optional stage graph filter; default removes `task_state: message_processing: tool_args:`)
- `COORDINATOR_DEMO_INCLUDE_CLICKUP` / `COORDINATOR_DEMO_INCLUDE_NOTION` (advanced override for specialist loading)

Note: If no usable specialist is available for the request, coordinator may return
`TASK_STATE_INPUT_REQUIRED` for clarification. The demo sink treats this as a successful handoff state.

Latency note: coordinator handoff time is mostly LLM time. If the run feels slow, reduce
`TASK_DAEMON_DEMO_MAX_CANDIDATES` (for example `2` or `3`) to shrink workflow breadth.

ClickUp grouping note: the current `support/clickup` tool does not set workspace-specific
custom fields (such as a `Project` dropdown). New tasks may appear under ClickUp's `Empty`
grouping in some views. This is a known limitation and should be addressed with explicit,
typed custom-field support rather than implicit environment-driven behavior.

## Failure Plan

1. If OpenRouter is degraded, use local fallback provider (`TASK_DAEMON_LLM_FALLBACK_*`).
2. If Slack auth fails, rerun OAuth exchange and verify channel access (`#agentium-eng`).
   - For private channels, set `TASK_DAEMON_DEMO_CHANNEL=<C...>` and `TASK_DAEMON_DEMO_AUTH=user`.
3. If coordinator boot fails, run `just coordinator-demo-stop` then retry `just task-daemon-demo`.
4. If context capture fails, inspect `/tmp/task-daemon-demo.log` and `/tmp/coordinator-runner.log`.
5. If visual artifacts are missing, rerun export only using `TASK_DAEMON_DEMO_CONTEXT_ID=<ctx-id> TASK_DAEMON_DEMO_SKIP_POLL=1 TASK_DAEMON_DEMO_START_COORDINATOR=0 TASK_DAEMON_DEMO_RESET_STATE=0 just task-daemon-demo`.
