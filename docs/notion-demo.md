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

## Demo Narrative (Recommended)

For team demos, run these scenes in order:

1. `Search scene`: `just notion-demo`
2. `Deterministic scene`: `NOTION_DEMO_PAGE_ID="<page-or-block-id>" just notion-demo`
3. `Trace scene`: run `just provenance-mermaid <context_id>` from either prior run and inspect the sequence narrative

This sequence highlights planning behavior, deterministic direct-ID behavior, and
provenance-backed observability in one arc.

## Useful Environment Overrides

- `NOTION_DEMO_PORT` (default `8080`)
- `NOTION_DEMO_PROVENANCE_DB` (default `provenance.db`)
- `NOTION_DEMO_LOG` (default `/tmp/notion-runner.log`)
- `NOTION_DEMO_PID` (default `/tmp/notion-runner.pid`)
- `NOTION_DEMO_STREAM` (default `/tmp/notion-demo-sse.log`)
- `NOTION_DEMO_RUNNER_URL` (default `http://127.0.0.1:${NOTION_DEMO_PORT}`)
- `NOTION_DEMO_REPOSITORY_URL` (default `${NOTION_DEMO_RUNNER_URL}/repository`)
- `NOTION_DEMO_STATE_DIR` / `NOTION_DEMO_REPOSITORY_DIR` (defaults under `/tmp/notion-demo-*-${PORT}`)
- `NOTION_DEMO_BUILDER_BIN` (default `${CARGO_TARGET_DIR:-target}/debug/baml-agent-builder`)
- `NOTION_DEMO_RUNNER_BIN` (default `${CARGO_TARGET_DIR:-target}/debug/baml-agent-runner`)
