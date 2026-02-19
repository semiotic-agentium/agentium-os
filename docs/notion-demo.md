# Notion Demo (Auditable, Provenance-First)

This demo is designed to reflect the runtime's core design:

- Host tools execute as Rust session plans (`Open -> Send -> Next -> Finish`)
- Notion access is read-only
- Conversation + tool events are captured in provenance for replay

## Prerequisites

- `.env` contains:
  - `OPENROUTER_API_KEY`
  - `NOTION_API_TOKEN`
- `jq` installed

## Quick Start

```bash
just notion-demo
```

This command:

1. Packages `agents/notion-agent`
2. Starts `baml-agent-runner` in HTTP mode
3. Enables file-backed provenance (default: `provenance.db`)
4. Sends one `message.sendStream` request to `/agents/notion-agent/default/a2a/sse`
5. Captures SSE output and attempts to print `contextId`/`taskId`

Stop the background runner:

```bash
just notion-demo-stop
```

## Typical Scenarios

Use default prompt (search + summarize):

```bash
just notion-demo
```

Run a page-targeted summary (deterministic path):

```bash
NOTION_DEMO_PAGE_ID="<notion-page-or-block-id>" just notion-demo
```

Custom text:

```bash
NOTION_DEMO_TEXT="Summarize this Notion page <id> with sources." just notion-demo
```

## Provenance Replay

If `notion-demo` prints a `contextId`, export the sequence diagram:

```bash
just provenance-mermaid <context_id>
```

The diagram should show the runtime narrative end-to-end:

- User request
- LLM planning
- Tool invocation/return
- Agent response

## Useful Environment Overrides

- `NOTION_DEMO_PORT` (default `8081`)
- `NOTION_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `NOTION_DEMO_LOG` (default `/tmp/notion-runner.log`)
- `NOTION_DEMO_PID` (default `/tmp/notion-runner.pid`)
- `NOTION_DEMO_STREAM` (default `/tmp/notion-demo-sse.log`)
- `NOTION_DEMO_PACKAGE` (default `/tmp/notion-agent.tar.gz`)
- `NOTION_DEMO_RUNNER_BIN` (default `target/debug/baml-agent-runner`)
