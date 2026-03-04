# Task Daemon Interpretation Behavior

This document describes the product behavior we want from `baml-task-daemon`.

The daemon should read a project Slack channel and answer one practical question:
"What should we do next, and why?"

## Outcome Standard

For each poll window, the daemon should produce:
- a clear project-state summary
- concrete investigations for code-aware follow-through
- explicit clarifications when discussion is ambiguous or blocked
- follow-up prompts when no code action is currently possible

## User-Centered Contract

The output should be useful to three audiences at the same time:
- humans reviewing channel state
- orchestration agents planning next actions
- task systems (for example ClickUp) tracking execution

A simplified output shape:

```json
{
  "source_label": "#project-channel",
  "project": {
    "project_key": "agent-platform",
    "repo_available": true
  },
  "interpretation": {
    "executive_summary": "...",
    "decisions_made": ["..."],
    "open_questions": ["..."],
    "risks": ["..."],
    "workflow_seed": {
      "goal": "...",
      "investigation_nodes": ["..."],
      "clarification_nodes": ["..."],
      "follow_up_nodes": ["..."]
    }
  },
  "derived_tasks": ["..."]
}
```

## Interpretation Principles

- Interpret meaning, not keywords.
- Prefer project context over message-local phrasing.
- Include evidence references for non-trivial claims.
- Acknowledge uncertainty explicitly instead of fabricating confidence.
- If repo details are unknown, emit clarification or follow-up nodes rather than invented code assertions.

## Workflow Seed Semantics

`workflow_seed` is the handoff surface for orchestration.

- `goal`: what successful follow-through should accomplish now
- `investigation_nodes`: concrete investigative actions (codebase-aware when repo exists)
- `clarification_nodes`: questions required to de-risk or unblock execution
- `follow_up_nodes`: stakeholder or decision actions when code action is premature

## Operational Behavior

- Incremental polling over channel history
- Idempotency via persisted state and stable task keys
- Read-only Slack interaction
- At-most-once delivery semantics at sink boundary

## Quality Bar

A high-quality batch should let a teammate or agent start work immediately without re-reading the full thread.

Signs of quality:
- summary reflects the real thread arc
- investigations are specific and testable
- blocking questions are clearly separated from non-blocking ambiguity
- risks include impact, not just labels
- generated tasks are coherent with the interpretation, not generic TODOs
