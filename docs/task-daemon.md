# Task Daemon User Guide

`baml-task-daemon` polls project work sources and emits actionable outputs for humans and agents. Use repeatable `--source` flags to select sources (for example `--source slack --source clickup`).

Each poll produces:
- a project interpretation (what changed, decisions, risks, open questions)
- a workflow seed (investigations, clarifications, follow-ups)
- derived tasks (for ClickUp or other consumers)

## Who this is for

Use this when project coordination lives in Slack and/or ClickUp and you want faster follow-through without manual triage.

## Quick Start

1. Set credentials for the sources you plan to run.

```bash
# Slack source mode
export SLACK_BOT_TOKEN=xoxb-...

# ClickUp source mode
export CLICKUP_API_KEY=pk_...
```

2. Choose an LLM provider.

Remote provider example:

```bash
export OPENROUTER_API_KEY=...
```

Local OpenAI-compatible provider example (LM Studio, Ollama OpenAI bridge, etc.):

```bash
export TASK_DAEMON_LLM_BASE_URL=http://localhost:1234/v1
export TASK_DAEMON_LLM_MODEL=<your-local-model>
```

3. Run one poll (Slack mode shown).

```bash
cargo run -p baml-task-daemon -- run --channel agentium-eng --once
```

4. Run continuously.

```bash
cargo run -p baml-task-daemon -- run --channel agentium-eng --interval-seconds 120
```

## Common Usage Patterns

Write machine-consumable JSONL for downstream automation:

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --jsonl-out /tmp/task-daemon.jsonl
```

Map channel to project metadata (repo path, ClickUp list):

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --project-config .agentium/task-daemon-projects.json
```

Enable ClickUp sink:

```bash
# dry-run (safe default)
cargo run -p baml-task-daemon -- run --channel agentium-eng --clickup-list-id <LIST_ID>

# live writes
cargo run -p baml-task-daemon -- run --channel agentium-eng --clickup-list-id <LIST_ID> --clickup-live
```

Use ClickUp as the source input (task-created/task-terminal/task-removed lifecycle events):

```bash
# ClickUp source only
cargo run -p baml-task-daemon -- run \
  --source clickup \
  --clickup-list-id <LIST_ID> \
  --once

# Poll Slack and ClickUp in the same daemon loop
cargo run -p baml-task-daemon -- run \
  --source slack \
  --source clickup \
  --channel agentium-eng \
  --clickup-list-id <LIST_ID>
```

Delegate to coordinator agent over A2A:

```bash
# dry-run (logs request payload intent, no network side-effects)
cargo run -p baml-task-daemon -- run --channel agentium-eng --coordinator-url http://127.0.0.1:8082

# live delegation
cargo run -p baml-task-daemon -- run --channel agentium-eng --coordinator-url http://127.0.0.1:8082 --a2a-live
```

## Output You Should Expect

A typical batch includes:
- `interpretation.executive_summary`: concise state of the project discussion
- `interpretation.decisions_made`, `open_questions`, `risks`: structured understanding of the conversation
- `interpretation.workflow_seed.goal`: the intended next objective
- `interpretation.workflow_seed.investigation_nodes`: high-agency investigation prompts with evidence references
- `interpretation.workflow_seed.clarification_nodes`: questions that should be resolved before execution
- `derived_tasks`: practical tasks emitted to sinks

When using coordinator delegation (`--coordinator-url --a2a-live`), task-daemon
sends a valid `message.sendStream` request with:
- a concise text instruction
- a typed workflow handoff payload in `message.parts[].data`

## Event Contract

For integration between poller, interpreter, and orchestration layers, use the
versioned interpretation event contract:
- [task-daemon-event-contract.md](./task-daemon-event-contract.md)
- [task-daemon-clickup-source-contract.md](./task-daemon-clickup-source-contract.md) (ClickUp lifecycle source semantics)

For a leadership-focused end-to-end demo flow (Slack -> coordinator handoff ->
provenance timeline + mermaid), see:
- [task-daemon-demo.md](./task-daemon-demo.md)

## Important Behavior

- LLM mode is the default. Heuristic mode is available only when explicitly requested (`--extractor heuristic`).
- Slack and ClickUp source access are read-only.
- With multiple `--source` flags, each interval (and `--once`) covers each selected source once.
- Startup validation rejects configurations where a selected source has no compatible sink.
- ClickUp source lifecycle semantics (keys, revisions, loop-prevention) are defined in [task-daemon-clickup-source-contract.md](./task-daemon-clickup-source-contract.md).
- Delivery is currently best-effort at-least-once: source cursor/task state is persisted only after sink delivery succeeds.

## Minimal Project Config Example

```json
{
  "channels": {
    "agentium-eng": {
      "project_key": "agent-platform",
      "repo_path": "/path/to/agent-platform",
      "clickup_list_id": "901325431486"
    }
  }
}
```
