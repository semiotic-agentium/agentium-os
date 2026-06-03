# Coordinator Demo (Dynamic Delegation Path)

This demo shows a host-level coordinator agent that discovers available tools/agents,
routes to the best specialist (Notion or ClickUp), delegates through
`system/internal_a2a`, then synthesizes a user-facing answer with
confidence and gaps.

## What this demonstrates

- Delegation-first orchestration (`coordinator-agent -> specialist-agent`)
- Routing plan from `system/discover_agents` + `system/discover_tools`
- Bounded autonomy (max 5 coordinator planning steps)
- High-agency behavior (auto-drills into a Notion page when search results are only a listing)
- Auditable output contract (`answer`, `actionable goals`, `sources`, `confidence`, `gaps`, optional clarification)

## Prerequisites

- `.env` contains:
  - `OPENROUTER_API_KEY`
  - `NOTION_API_TOKEN` (if Notion specialist enabled)
  - `CLICKUP_API_KEY` (if ClickUp specialist enabled)
- `jq` installed

Known limitation: the current `support/clickup` tool does not set workspace-specific custom
fields (for example a `Project` dropdown), so created tasks may appear in generic ClickUp
groupings depending on workspace view configuration.

## Quick Start (CLI stream)

```bash
just coordinator-demo
```

To include ClickUp specialist routing in the same runner:

```bash
COORDINATOR_DEMO_INCLUDE_CLICKUP=1 just coordinator-demo
```

To run ClickUp-first specialist routing (without Notion):

```bash
COORDINATOR_DEMO_INCLUDE_CLICKUP=1 COORDINATOR_DEMO_INCLUDE_NOTION=0 just coordinator-demo
```

To run coordinator-only (no Notion specialist loaded):

```bash
COORDINATOR_DEMO_INCLUDE_NOTION=0 just coordinator-demo
```

This command:

1. Packages `agents/coordinator-agent` and `agents/notion-agent` when enabled
2. Starts `baml-agent-runner` in HTTP mode
3. Loads the packaged agents into one host
4. Sends one `message.sendStream` request to `/agents/coordinator-agent/default/a2a`
5. Prints `contextId`/`taskId` when present

Stop the background runner:

```bash
just coordinator-demo-stop
```

## UI Mode (existing chat UI)

Start the backend without sending the scripted request:

```bash
COORDINATOR_DEMO_NO_STREAM=1 just coordinator-demo
```

Then in the chat UI:

1. Point backend/API to `http://127.0.0.1:8082`
2. Select `coordinator-agent` in the agent dropdown
3. Ask:
   - `Can you tell me what the research team are up to and what actionable goals they have?`
   - `Use ClickUp and summarize actionable goals for this sprint.` (works when a `clickup-agent` package is loaded in the same runner)

The coordinator will discover capabilities, choose a specialist route, and either:
- Delegate and synthesize results, or
- Ask a focused clarification when routing is ambiguous.

## Provenance replay

If `coordinator-demo` prints a `contextId`, fetch the Mermaid sequence from the runner
(`COORDINATOR_DEMO_RUNNER_URL`, default `http://127.0.0.1:8082` unless overridden):

```bash
curl -sS "${COORDINATOR_DEMO_RUNNER_URL:-http://127.0.0.1:8082}/contexts/<context_id>/mermaid"
```

## Useful environment overrides

- `COORDINATOR_DEMO_PORT` (default `8082`)
- `COORDINATOR_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `COORDINATOR_DEMO_LOG` (default `/tmp/coordinator-runner.log`)
- `COORDINATOR_DEMO_PID` (default `/tmp/coordinator-runner.pid`)
- `COORDINATOR_DEMO_STREAM` (default `/tmp/coordinator-demo-sse.log`)
- `COORDINATOR_DEMO_ENTRY_AGENT` (default `coordinator-agent`)
- `COORDINATOR_DEMO_RUNNER_URL` (default `http://127.0.0.1:${COORDINATOR_DEMO_PORT}`)
- `COORDINATOR_DEMO_REPOSITORY_URL` (default `${COORDINATOR_DEMO_RUNNER_URL}/repository`)
- `COORDINATOR_DEMO_STATE_DIR` / `COORDINATOR_DEMO_REPOSITORY_DIR` (defaults under `/tmp/coordinator-demo-*-${PORT}`)
- `COORDINATOR_DEMO_BUILDER_BIN` (default `${CARGO_TARGET_DIR:-target}/debug/baml-agent-builder`)
- `COORDINATOR_DEMO_RUNNER_BIN` (default `${CARGO_TARGET_DIR:-target}/debug/baml-agent-runner`)
- `COORDINATOR_DEMO_INCLUDE_CLICKUP` (`1` to also publish/deploy `clickup-agent`)
- `COORDINATOR_DEMO_INCLUDE_NOTION` (default `1`; set `0` for coordinator-only)
- `COORDINATOR_DEMO_TEXT` (override prompt text)
- `COORDINATOR_DEMO_NO_STREAM` (`1` to start backend only)
