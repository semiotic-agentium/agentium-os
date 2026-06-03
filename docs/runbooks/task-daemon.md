# Task Daemon User Guide

`baml-task-daemon` watches a Slack channel for a project and turns conversation into actionable outputs for humans and agents.

Architecturally, task-daemon should be understood as an event publisher.

- The host publishes structured events.
- Agents declare which events they want.
- The host matches published events to subscribed agents.
- The host delivers the event into an agent task/context.
- The receiving agent decides what to do.

That means task-daemon should stay focused on:

- source polling
- dedupe and persisted state
- event emission

It should not become the place where downstream workflow policy lives.

That is now a compatibility shape, not the only target model. The longer-term direction is host-managed source polling plus source-family semantic ingress agents that interpret raw events after delivery.

Each poll produces:
- a project interpretation (what changed, decisions, risks, open questions)
- a workflow seed (investigations, clarifications, follow-ups)
- derived tasks (for ClickUp or other consumers)

## Core Concepts

- `capabilities`: what an agent can do
- `subscriptions`: which published events an agent wants delivered

Those are different concerns. Using capabilities as a proxy for event interest
creates ambiguity about whether the host is discovering execution skills or
deciding who should receive events.

`workflow_seed` is the handoff surface for orchestration:

- `goal`: what successful follow-through should accomplish now
- `investigation_nodes`: concrete investigative actions
- `clarification_nodes`: questions required to de-risk or unblock execution
- `follow_up_nodes`: stakeholder or decision actions when code action is premature

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

## Important Behavior

- LLM mode is the default. Heuristic mode is available only when explicitly requested (`--extractor heuristic`).
- Slack access is read-only.
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
