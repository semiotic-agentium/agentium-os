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
  - `NOTION_API_TOKEN`
- `jq` installed

## Quick Start (CLI stream)

```bash
just coordinator-demo
```

To include ClickUp specialist routing in the same runner:

```bash
COORDINATOR_DEMO_INCLUDE_CLICKUP=1 just coordinator-demo
```

This command:

1. Packages `agents/coordinator-agent` and `agents/notion-agent`
2. Starts `baml-agent-runner` in HTTP mode
3. Loads both agent packages into one host
4. Sends one `message.sendStream` request to `/agents/coordinator-agent/default/a2a/sse`
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

If `coordinator-demo` prints a `contextId`, export sequence diagram:

```bash
just provenance-mermaid <context_id>
```

## Useful environment overrides

- `COORDINATOR_DEMO_PORT` (default `8082`)
- `COORDINATOR_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `COORDINATOR_DEMO_LOG` (default `/tmp/coordinator-runner.log`)
- `COORDINATOR_DEMO_PID` (default `/tmp/coordinator-runner.pid`)
- `COORDINATOR_DEMO_STREAM` (default `/tmp/coordinator-demo-sse.log`)
- `COORDINATOR_DEMO_ENTRY_AGENT` (default `coordinator-agent`)
- `COORDINATOR_DEMO_PACKAGE` (default `/tmp/coordinator-agent.tar.gz`)
- `COORDINATOR_DEMO_NOTION_PACKAGE` (default `/tmp/notion-agent.tar.gz`)
- `COORDINATOR_DEMO_RUNNER_BIN` (default `target/debug/baml-agent-runner`)
- `COORDINATOR_DEMO_INCLUDE_CLICKUP` (`1` to also package/load `clickup-agent`)
- `COORDINATOR_DEMO_CLICKUP_PACKAGE` (default `/tmp/clickup-agent.tar.gz`)
- `COORDINATOR_DEMO_TEXT` (override prompt text)
- `COORDINATOR_DEMO_NO_STREAM` (`1` to start backend only)
