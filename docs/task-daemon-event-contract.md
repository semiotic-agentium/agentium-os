# Task Daemon Event Contract (Interpretation v1)

This contract defines the interpretation handoff between:
- Slack polling in `baml-task-daemon`
- conversation interpretation (LLM/agent)
- downstream orchestration and sinks (for example ClickUp)

The goal is to make every interpretation run traceable and replayable.

This `interpretation.v1` contract is currently Slack-message based. For ClickUp
source lifecycle semantics (`--source clickup`), see:
- [task-daemon-clickup-source-contract.md](./task-daemon-clickup-source-contract.md)

## Contract Version

- `schema_version`: `task-daemon.interpretation.v1`

## Request Event

`InterpretationRequestEvent` is the payload produced from one poll window and consumed by an interpreter.

Key fields:
- `event_id`: stable id for this request
- `source`: stable source identity (`source_key`, `source`, `source_label`)
- `project`: project context (`project_key`, repo availability/path)
- `messages`: normalized Slack messages
- `provenance`: optional runtime links (`context_id`, `task_id`, `correlation_id`, cursor, message timestamps)

Example:

```json
{
  "schema_version": "task-daemon.interpretation.v1",
  "event_id": "td-interpret-request-9782a72fddf4b5a5",
  "emitted_at_unix": 1772556104,
  "source": {
    "source_key": "slack:C123",
    "source": "slack",
    "source_label": "#agentium-eng"
  },
  "project": {
    "project_key": "agent-platform",
    "repo_available": true,
    "repo_path": "/Users/joseph/git/semiotic-agentium/agent-platform"
  },
  "provenance": {
    "context_id": "ctx-3cbbf",
    "correlation_id": "corr-20260303-001",
    "source_cursor": "1735689700.000000",
    "source_message_ts": [
      "1735689600.000000",
      "1735689700.000000"
    ]
  },
  "messages": [
    {
      "channel_name": "agentium-eng",
      "channel_id": "C123",
      "ts": "1735689600.000000",
      "text": "We should verify cursor semantics under sink failure",
      "source": {
        "reference": "slack://channel/C123/p1735689600000000",
        "channel_id": "C123",
        "message_ts": "1735689600.000000"
      }
    }
  ]
}
```

## Result Event

`InterpretationResultEvent` is the payload produced by interpretation and consumed by orchestration/sinks.

Key fields:
- `request_event_id`: links back to the request event
- `interpretation`: project-aware meaning of the discussion
- `derived_tasks`: executable tasks generated from interpretation
- `provenance.parent_event_id`: causality link to request `event_id`

Example:

```json
{
  "schema_version": "task-daemon.interpretation.v1",
  "event_id": "td-interpret-result-40e6d6c7d88bb18f",
  "request_event_id": "td-interpret-request-9782a72fddf4b5a5",
  "emitted_at_unix": 1772556105,
  "source": {
    "source_key": "slack:C123",
    "source": "slack",
    "source_label": "#agentium-eng"
  },
  "project": {
    "project_key": "agent-platform",
    "repo_available": true,
    "repo_path": "/Users/joseph/git/semiotic-agentium/agent-platform"
  },
  "messages_scanned": 1,
  "interpretation": {
    "executive_summary": "Team is deciding delivery guarantees and needs code-level validation.",
    "current_objectives": [
      "Validate cursor/save ordering against sink error paths"
    ],
    "workflow_seed": {
      "goal": "Confirm failure semantics and document expected behavior",
      "investigation_nodes": [],
      "clarification_nodes": [],
      "follow_up_nodes": []
    }
  },
  "provenance": {
    "context_id": "ctx-3cbbf",
    "correlation_id": "corr-20260303-001",
    "parent_event_id": "td-interpret-request-9782a72fddf4b5a5",
    "source_cursor": "1735689700.000000",
    "source_message_ts": [
      "1735689600.000000",
      "1735689700.000000"
    ]
  }
}
```

## Rust Types

Defined in [crates/task-daemon/src/contract.rs](/Users/joseph/git/semiotic-agentium/agent-platform/crates/task-daemon/src/contract.rs):
- `ContractSource`
- `ContractProvenance`
- `InterpretationRequestEvent`
- `InterpretationResultEvent`

These are also re-exported from `baml_task_daemon`.
