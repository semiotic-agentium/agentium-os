# Task Daemon ClickUp Source Contract

This contract defines the runtime semantics for `--source clickup` in `baml-task-daemon`.

## Scope

- Polls one or more configured ClickUp list ids.
- Reads list tasks with pagination and `include_closed=true` so terminal transitions remain observable.
- Produces inferred investigation tasks from lifecycle transitions.
- Persists per-source snapshots for idempotency/reconciliation.

## Source Identity

- `source`: `clickup`
- `source_key`: `clickup:<sorted_list_id_csv>`
- `source_label`:
  - single list: `clickup:list:<list_id>`
  - multiple lists: `clickup:lists:<sorted_list_id_csv>`

List ids are trimmed, deduplicated, and sorted before source identity is derived.

## Emitted Lifecycle Events

The ClickUp source emits derived task keys with these stable formats:

- Created:
  - key: `clickup-created:<task_id>`
  - recurrent key (same task re-created in monitored scope): `clickup-created:<task_id>:r<N>`
  - condition: task exists in current poll snapshot but did not exist in previous snapshot.
- Terminal transition:
  - key: `clickup-terminal:<task_id>:<normalized_terminal_status>`
  - recurrent key (same task hits terminal again after re-open): `clickup-terminal:<task_id>:<normalized_terminal_status>:r<N>`
  - condition: previous status was non-terminal, current status is terminal, and normalized status changed.
- Removed:
  - key: `clickup-removed:<task_id>`
  - recurrent key (same task removed again after re-add): `clickup-removed:<task_id>:r<N>`
  - condition: task existed in previous snapshot and is absent from current snapshot.

Terminal status matching is case-insensitive and currently treats status text containing any of:
`cancel`, `closed`, `complete`, `done`, `resolved` as terminal.

## Idempotency and Replay Semantics

- A stable snapshot of observed ClickUp tasks is stored per `source_key`.
- Derived task keys are deduped through `seen_task_keys` in daemon state.
- Delivery semantics are best-effort at-least-once:
  - state is persisted only after sink delivery succeeds.
  - sink failure causes retry of the same source window on the next poll.

## Loop Prevention

ClickUp sink (`--clickup-list-id`) does not accept batches whose `batch.source == clickup`.

The daemon skips incompatible sinks and fails delivery if no compatible sink is configured for a source batch.
This prevents recursive writes when ClickUp is both an input source and a sink target.

## Known Edge Behavior

- A task moved out of monitored lists appears as `clickup-removed:<task_id>`.
- If that task later appears in a monitored list, it appears as a new `clickup-created:<task_id>`.
- Task hard-deletes and list migration are intentionally collapsed into the same "removed from monitored scope" contract.
