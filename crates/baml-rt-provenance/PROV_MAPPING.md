# A2A Provenance Mapping (W3C PROV)

This document defines how `ProvEvent` is normalized into W3C PROV structures
and A2A-derived relations for storage in the provenance graph (SurrealDB).

## Node Labels and Identity

- Nodes are labeled by their `prov:type` (namespace stripped), e.g. `A2ATask`, `A2ATaskExecution`, `A2AMessage`.
- Base PROV kind is retained as `prov:base_type` with values `ProvEntity`, `ProvActivity`, or `ProvAgent`.
- Each node uses `name` as a stable identity key for upserts.

## ID Construction Semantics

Identifiers are constructed to match the semantics of the thing they model:

- **Derived IDs**: deterministic from domain identifiers (e.g. `task:<task_id>`,
  `message_processing:<message_id>`, `llm_call:<activity_anchor>`).
- **Runtime IDs**: agent runtime identity is `AgentId` (UUID) and anchors
  `agent:<agent_id>`, `agent_instance:<agent_id>`, `agent_boot:<agent_id>`.
- **Archive IDs**: `archive:<package_identity>` derives from package identity
  (manifest `name@version` or a package hash), never from temp extraction paths.
- **Runner ID**: `agent:runner` is a constant control-plane identity.

### Planning aliases (intent / plan / step)

Strings submitted on the execution-session wire (`intentId`, `planId`, `stepId`) and echoed in
planning query records are **not** global provenance identifiers. They are **task-scoped planning
aliases** (often agent-chosen or LLM-authored slugs). Canonical graph nodes for intent, plan, and
plan step entities use **derived** ids that compound `task_id` with those aliases — see
`IntentEntityId`, `PlanEntityId`, and `PlanStepEntityId` in `id_semantics.rs` (inputs include
`task_id` + alias).

For explicit mappings and intent, see:
`crates/baml-rt-provenance/src/id_semantics.rs` and
`crates/baml-rt-provenance/src/normalizer.rs`.

## Common Attributes

All nodes created from a `ProvEvent` include:

- `a2a:context_id` (string)
- `a2a:activity_anchor` (string)
- `a2a:task_id` (string, when available)

## Edge Properties

The graph backend supports relationship properties. We currently set:

- **PROV edges** (`USED`, `WAS_GENERATED_BY`, `WAS_ASSOCIATED_WITH`, `WAS_DERIVED_FROM`):
  only PROV fields (`prov:role`, `prov:time`, `prov:activity`, `prov:type` as applicable).
- **A2A-derived edges**: carry activity metadata (`a2a:context_id`, `a2a:activity_anchor`, `a2a:task_id`)
  plus any relation-specific attributes (e.g. `a2a:direction`, `a2a:relation`).

## Relation Naming Rule

All provenance relation labels are **PAST TENSE, PASSIVE VOICE**. This is PROV:
the events have already happened and are described as outcomes.

## W3C PROV Relations (Edges)

The underlying PROV directions are fixed:

- `USED` : `ProvActivity` -> `ProvEntity` (`prov:role` when provided)
- `WAS_GENERATED_BY` : `ProvEntity` -> `ProvActivity` (`prov:time` when provided)
- `USED` : `ProvActivity` -> `ProvEntity` (`prov:role` when provided)
- `WAS_DERIVED_FROM` : `ProvEntity` -> `ProvEntity` (`prov:activity`, `prov:type` when provided)

### Semantic Labels on PROV Edges (current)

These are labels applied **on top of** the PROV edges without changing direction:

- `USED` (activity -> entity):
  - `A2ATaskExecution` -> `Message` (role `input_message`) = `WAS_SPAWNED_BY`
  - `A2AMessageProcessing` -> `Message` (role `input_message`) = `WAS_RECEIVED_BY`
  - `LlmCall` -> `Message` (role `input_message`) = `WAS_CONSUMED_BY`
  - `ToolCall` -> `Message` (role `input_message`) = `WAS_CONSUMED_BY`
  - `A2ATaskExecution` -> `A2ATaskState` (role `task_state`) = `WAS_UPDATED_BY`
  - `LlmCall` -> `LlmPrompt` (role `a2a:prompt`) = `WAS_USED_BY`
  - `ToolCall` -> `ToolArgs` (role `a2a:args`) = `WAS_USED_BY`
  - `ToolCall` -> `DelegationTarget` (role `a2a:delegation_target`) = `WAS_DELEGATED_TO`
  - `AgentBoot` -> `AgentArchive` (role `a2a:archive`) = `WAS_BOOTSTRAPPED_BY`
- `WAS_GENERATED_BY` (entity -> activity):
  - `Message` -> `A2AMessageProcessing` = `WAS_EMITTED_BY`
  - `Artifact` -> `A2ATaskExecution` = `WAS_GENERATED_BY`
  - `A2ATask` -> `A2ATaskExecution` = `WAS_CREATED_BY`
  - `AgentRuntimeInstance` -> `AgentBoot` = `WAS_SPAWNED_BY`
- `WAS_ASSOCIATED_WITH` (activity -> agent runtime instance):
  - `prov:role = executing_agent` = `WAS_EXECUTED_BY`
  - `prov:role = invoking_agent` = `WAS_INVOKED_BY`
  - `prov:role = calling_agent` = `WAS_CALLED_BY`
- `WAS_DERIVED_FROM` (entity -> entity):
  - `prov:type = a2a:status_transition` = `WAS_TRANSITIONED_FROM`

## A2A-Derived Relations (Edges)

These edges are added to make A2A domain queries easier while keeping PROV
semantics intact.

### Derived Relation Directions (current)

- `A2A_TASK_CALL` : `A2ATaskExecution` -> `LlmCall` = `WAS_INVOKED_BY`
- `A2A_TASK_CALL` : `A2ATaskExecution` -> `ToolCall` = `WAS_EXECUTED_BY`
- `A2A_MESSAGE_CALL` : `A2AMessageProcessing` -> `LlmCall` = `WAS_INVOKED_BY`
- `A2A_MESSAGE_CALL` : `A2AMessageProcessing` -> `ToolCall` = `WAS_EXECUTED_BY`
- `A2A_TASK_MESSAGE` : `A2ATask` -> `Message`
  - direction `received` = `WAS_SPAWNED_BY`
  - direction `sent` = `WAS_EMITTED_BY`
- `A2A_TASK_ARTIFACT` : `A2ATask` -> `Artifact` = `WAS_GENERATED_BY`
- `A2A_TASK_STATUS_TRANSITION` : `A2ATaskState(old)` -> `A2ATaskState(new)` = `WAS_TRANSITIONED_TO`

## Event-to-PROV Mapping

| ProvEventType | Nodes | PROV Edges (direction) | Derived Edges (direction) |
| --- | --- | --- | --- |
| `LlmCallStarted` | `LlmCall` activity, `LlmPrompt` entity, optional `Message` entity | `LlmCall` -> `LlmPrompt` (`WAS_USED_BY`), optional `LlmCall` -> `Message` (`WAS_CONSUMED_BY`) | `A2ATaskExecution` -> `LlmCall` (`WAS_INVOKED_BY`) or `A2AMessageProcessing` -> `LlmCall` (`WAS_INVOKED_BY`) |
| `LlmCallCompleted` | `LlmCall` activity, `LlmPrompt` entity, optional `Message` entity | `LlmCall` -> `LlmPrompt` (`WAS_USED_BY`), optional `LlmCall` -> `Message` (`WAS_CONSUMED_BY`) | `A2ATaskExecution` -> `LlmCall` (`WAS_INVOKED_BY`) or `A2AMessageProcessing` -> `LlmCall` (`WAS_INVOKED_BY`) |
| `ToolCallStarted` | `ToolCall` activity, `ToolArgs` entity, optional `Message` entity | `ToolCall` -> `ToolArgs` (`WAS_USED_BY`), optional `ToolCall` -> `Message` (`WAS_CONSUMED_BY`) | `A2ATaskExecution` -> `ToolCall` (`WAS_EXECUTED_BY`) or `A2AMessageProcessing` -> `ToolCall` (`WAS_EXECUTED_BY`) |
| `ToolCallCompleted` | `ToolCall` activity, `ToolArgs` entity, optional `Message` entity | `ToolCall` -> `ToolArgs` (`WAS_USED_BY`), optional `ToolCall` -> `Message` (`WAS_CONSUMED_BY`) | `A2ATaskExecution` -> `ToolCall` (`WAS_EXECUTED_BY`) or `A2AMessageProcessing` -> `ToolCall` (`WAS_EXECUTED_BY`) |
| `TaskExists` | `A2ATask` entity | Idempotent upsert | — |
| `TaskExecutionStarted` | `A2ATaskExecution` activity | `A2ATask` -> `A2ATaskExecution` (`WAS_CREATED_BY`), `A2ATaskExecution` -> `AgentRuntimeInstance` (`WAS_EXECUTED_BY`/`WAS_INVOKED_BY`) | — |
| `TaskExecutionEnded` | `A2ATaskExecution` end_time | Updates existing activity | — |
| `TaskStatusChanged` | `A2ATaskExecution` activity, `A2ATaskState` entities | `A2ATaskExecution` -> `A2ATaskState` (`WAS_UPDATED_BY`), `A2ATaskState(new)` -> `A2ATaskState(old)` (`WAS_DERIVED_FROM` / STATUS_TRANSITION). One node per (task_id, status); idempotent MERGE. | — |
| `TaskArtifactGenerated` | `A2ATaskExecution` activity, `Artifact` entity, `A2ATask` entity | `Artifact` -> `A2ATaskExecution` (`WAS_GENERATED_BY`) | `A2ATask` -> `Artifact` (`WAS_GENERATED_BY`) |
| `MessageReceived` | `A2AMessageProcessing` activity, `Message` entity, `A2ATask` entity | `A2AMessageProcessing` -> `Message` (`WAS_RECEIVED_BY`), `A2AMessageProcessing` -> `Agent` (`WAS_EXECUTED_BY`/`WAS_INVOKED_BY`) | `A2ATask` -> `Message` (`WAS_SPAWNED_BY`) |
| `MessageSent` | `A2AMessageProcessing` activity, `Message` entity, `A2ATask` entity | `Message` -> `A2AMessageProcessing` (`WAS_EMITTED_BY`), `A2AMessageProcessing` -> `Agent` (`WAS_EXECUTED_BY`/`WAS_INVOKED_BY`) | `A2ATask` -> `Message` (`WAS_EMITTED_BY`) |

## Notes

- Task entities and task execution activities are created on-demand when task-linked events arrive.
- `name` (and the graph `id` property) is used for deterministic upserts.

## Validation

The provenance store is backed by SurrealDB. Use the store’s
read API or execute SurrealQL queries to validate the
populated graph.
