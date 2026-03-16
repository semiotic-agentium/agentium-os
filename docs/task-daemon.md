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

When a manifest declares `subscriptions[].source_kinds`, use the canonical
source-kind identifiers emitted by task-daemon:

- `slack`
- `clickup`
- `github_issues`

Matching lowercases input, but it does not rewrite punctuation or separators.
For example, `github_issues` matches `GitHub_Issues`, but not `githubissues` or
`github-issues`.

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

Poll ClickUp as a source:

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

Route specific sources to specific sinks:

```bash
# Slack discussions create ClickUp tasks; ClickUp lifecycle events dispatch to subscribed agents.
cargo run -p baml-task-daemon -- run \
  --source slack \
  --source clickup \
  --channel agentium-eng \
  --clickup-list-id <LIST_ID> \
  --dispatch-base-url http://127.0.0.1:8082 \
  --route slack:clickup \
  --route clickup:dispatch \
  --clickup-live \
  --dispatch-live
```

Deliver daemon events to subscribed agents:

```bash
# dry-run (shows what would be sent, without network side-effects)
cargo run -p baml-task-daemon -- run --channel agentium-eng --dispatch-base-url http://127.0.0.1:8082

# live delivery to subscribed agents discovered from the host /agents API
cargo run -p baml-task-daemon -- run --channel agentium-eng --dispatch-base-url http://127.0.0.1:8082 --dispatch-live
```

Migration note:

- `--dispatch-base-url` no longer implies delivery to `coordinator-agent/default`.
- The default is now subscriber discovery via the host `/agents` API.
- Existing invocations that relied on the old implicit single-target behavior
  should add both:
  - `--dispatch-agent-package <package>`
  - `--dispatch-agent-instance-id <instance>`

Override subscriber delivery and send to one explicit target:

```bash
cargo run -p baml-task-daemon -- run \
  --channel agentium-eng \
  --dispatch-base-url http://127.0.0.1:8082 \
  --dispatch-agent-package workflow-intake-agent \
  --dispatch-agent-instance-id default \
  --dispatch-live
```

## Output You Should Expect

A typical batch includes:
- `interpretation.executive_summary`: concise state of the project discussion
- `interpretation.decisions_made`, `open_questions`, `risks`: structured understanding of the conversation
- `interpretation.workflow_seed.goal`: the intended next objective
- `interpretation.workflow_seed.investigation_nodes`: high-agency investigation prompts with evidence references
- `interpretation.workflow_seed.clarification_nodes`: questions that should be resolved before execution
- `derived_tasks`: practical tasks emitted to sinks

When using agent delivery (`--dispatch-base-url --dispatch-live`), task-daemon
sends a buffered dispatch request with:
- `routing_key` set to a source-specific intake key such as `slack:intake`
- `message_type` set to `task-daemon.interpretation.v1`
- the typed workflow handoff payload in `messages[]`

## Event Contract

For integration between poller, interpreter, and orchestration layers, use the
versioned interpretation event contract:
- [task-daemon-event-contract.md](./task-daemon-event-contract.md)

## Important Behavior

- LLM mode is the default. Heuristic mode is available only when explicitly requested (`--extractor heuristic`).
- Slack and ClickUp source access are read-only.
- With multiple `--source` flags, each interval (and `--once`) covers each selected source once.
- `--route <source>:<sink>` overrides default fan-out. When routes are present, only explicitly routed source/sink pairs are active.
- Startup validation rejects configurations where a selected source has no compatible sink.
- With `--dispatch-base-url` and no explicit `--dispatch-agent-package` / `--dispatch-agent-instance-id`, task-daemon discovers subscribed agents from the host `/agents` API and delivers matching events to their `/dispatch` entrypoint.
- This replaces the older implicit single-target coordinator delivery. Use both explicit target flags if you need that behavior.
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
