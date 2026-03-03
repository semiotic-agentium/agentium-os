# Task Daemon User Guide

`baml-task-daemon` watches a Slack channel for a project and turns conversation into actionable outputs for humans and agents.

Each poll produces:
- a project interpretation (what changed, decisions, risks, open questions)
- a workflow seed (investigations, clarifications, follow-ups)
- derived tasks (for ClickUp or other consumers)

## Who this is for

Use this when a project channel has meaningful technical coordination and you want faster follow-through without manually triaging every message.

## Quick Start

1. Set Slack credentials.

```bash
export SLACK_BOT_TOKEN=xoxb-...
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

3. Run one poll.

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

## Output You Should Expect

A typical batch includes:
- `interpretation.executive_summary`: concise state of the project discussion
- `interpretation.decisions_made`, `open_questions`, `risks`: structured understanding of the conversation
- `interpretation.workflow_seed.goal`: the intended next objective
- `interpretation.workflow_seed.investigation_nodes`: high-agency investigation prompts with evidence references
- `interpretation.workflow_seed.clarification_nodes`: questions that should be resolved before execution
- `derived_tasks`: practical tasks emitted to sinks

## Event Contract

For integration between poller, interpreter, and orchestration layers, use the
versioned interpretation event contract:
- [docs/task-daemon-event-contract.md](/Users/joseph/git/semiotic-agentium/agent-platform/docs/task-daemon-event-contract.md)

## Important Behavior

- LLM mode is the default. Heuristic mode is available only when explicitly requested (`--extractor heuristic`).
- Slack access is read-only.
- Delivery is currently at-most-once: if sink delivery fails, already-polled messages are not replayed.

## Minimal Project Config Example

```json
{
  "channels": {
    "agentium-eng": {
      "project_key": "agent-platform",
      "repo_path": "/Users/joseph/git/semiotic-agentium/agent-platform",
      "clickup_list_id": "901325431486"
    }
  }
}
```
