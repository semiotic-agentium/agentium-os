// Unified step-executor harness: `ChooseClickUpAction` is a *SessionPlan root — `runGeneratedStepExecutor`
// is the same Rust-hosted mechanism as coordinator planning (prefix, transcript, intra supplement); IR here
// narrows the host-tool Open/active phases instead of unified-primary `FuncName__entry` only.

/// Phase 1 — Intent inference.
/// Classifies whether the message is a valid ClickUp request, asks for clarification
/// if ambiguous, rejects non-ClickUp requests, or distills a clean intent statement.
class NeedClarification {
  kind "clarify"
  question string @description("A clarifying question when the request is too vague to act on.")
}

class NotRelevant {
  kind "not_relevant"
  reason string @description("Why this message isn't a ClickUp request.")
}

class ClickUpIntent {
  kind "intent"
  intent string @description("Clean, distilled goal statement — what the user wants to do in ClickUp.")
  operation_kind "read" | "write" | "delete" @description("Broad category: read (list/get), write (create/update), delete.")
}

function InferClickUpIntent() -> NeedClarification | NotRelevant | ClickUpIntent {
  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are the intent classifier for a ClickUp workspace assistant.
    Your only job is to categorise the user's message — do NOT fetch data or execute actions.

    A message is a ClickUp request whenever it mentions tasks, lists, spaces, workspaces, assignments,
    due dates, statuses, priorities, or any project-management concept — whether the user wants to
    READ data (list, count, search, summarise) or WRITE data (create, update, delete).

    Decision rules:
    1. Treat the request as a real ClickUp intent when the message is about tasks, lists, spaces, or workspace state.
       - intent: plain-English goal statement (normalised, no filler)
       - operation_kind: "read" for any query/count/search; "write" for create/update; "delete" for delete
    2. Ask one clarifying question when the message relates to project management but is too vague
       to plan (e.g. "do the thing", "fix it", "help me" with no task or workspace context).
    3. Mark it not relevant ONLY when the message has NO connection whatsoever to project management,
       tasks, workspaces, or lists (e.g. "write me a poem", "what is the weather today?").

    Examples:
    - "how many tasks are in progress?" → count in-progress tasks, operation_kind "read"
    - "what is the total number of open tasks?" → count open tasks, operation_kind "read"
    - "what tasks are in progress?" → list in-progress tasks, operation_kind "read"
    - "list tasks assigned to me" → list tasks assigned to me, operation_kind "read"
    - "create a task called Deploy v2 in the backend list" → create that task, operation_kind "write"
    - "delete the task fix login bug" → delete that task, operation_kind "delete"
    - Host lifecycle ingress batches (clickup-created / clickup-terminal / clickup-removed record keys) →
      reconcile via read or write tool passes; prefer "read" to verify new or terminal tasks, "write" only when
      an explicit update is required; NEVER not_relevant when the message describes ClickUp lifecycle records.
    - "do the thing" → ask which task or workspace they mean
    - "send an email to my boss" → not relevant to ClickUp tasks or workspaces
    - "what is the weather?" → not relevant to ClickUp tasks or workspaces

    CRITICAL: Any question about task counts, status summaries, or workspace data is ALWAYS
    a ClickUp read intent — NEVER not relevant.

    CONTINUATION RULE: If the conversation history below shows a previous validated ClickUp intent and the
    current message is a follow-up or continuation request (e.g. "continue", "proceed", "keep going",
    "fetch the data", "use tool calls to finish"), carry forward the SAME intent and
    operation_kind as the prior turn — do NOT ask again.

    Classify from Session history above (host ingress poll and per-unit task lines).

{{ ctx.output_format }}

Return exactly one JSON object.
Select the object shape with discriminator `kind`:
- `kind: "clarify"` -> NeedClarification
- `kind: "not_relevant"` -> NotRelevant
- `kind: "intent"` -> ClickUpIntent
Set `kind` exactly. Do not mix fields from different object shapes.
Do not add text before or after the JSON object.
"#
}

/// One row in the ClickUp host plan — wire keys match coordination `StandardAgentPlanStep`.
class ClickUpPlanStep {
  agent_package string @description("Sentinel only: clickup-execute (one ClickUp tool session pass: navigate/list/get/create/update/delete) or clickup-format (final user answer from session evidence; no new API calls). Never clickup-agent, support/clickup, or discovery agent_package names.")
  agent_instance_id string @description("Always the literal default for this host: default")
  sub_message string @description("clickup-execute: concrete executor instruction (IDs, operation, limits). clickup-format: how to frame the final reply (counts, table, summary).")
}

/// ClickUp host structured plan — top-level keys align with shared StandardStructuredPlan for coordination code.
class ClickUpStructuredPlan {
  intent_description string @description("One sentence: user-visible outcome (not low-level API mechanics).")
  objective string @description("One sentence: what this turn achieves (may echo intent).")
  plan_steps ClickUpPlanStep[] @description("Ordered steps: at least one clickup-execute, then exactly one clickup-format as the LAST step. Extra clickup-execute rows only when separate tool passes are needed.")
  citations string[]? @description("Optional #N / @N line refs for observability when grounded in history; otherwise omit or [].")
}

/// Phase 2 — Plan ClickUp work. Contract is in `ClickUpStructuredPlan` / `ClickUpPlanStep` and `{{ ctx.output_format }}`.
function PlanClickUpWork(intent: string, operation_kind: string) -> ClickUpStructuredPlan {
  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are the planner for a ClickUp workspace assistant.
    Validated intent: {{ intent }}
    Operation kind: {{ operation_kind }}
    Plan only — do NOT call tools or fetch data.

    Output ClickUpStructuredPlan (not StandardStructuredPlan):
    - intent_description — one sentence, user-visible outcome (not low-level API steps).
    - objective — one sentence, what we achieve this turn (may echo intent).
    - plan_steps — ordered ClickUpPlanStep rows. For THIS host only (not multi-agent delegation):

    SENTINEL agent_package values (required — never use clickup-agent, support/clickup, or names from discover_agents):
    - clickup-execute — one executor pass: navigate/list/get/create/update/delete via the ClickUp tool session.
      sub_message = concrete instruction for this hop (which IDs to discover, which operation, limits, task_id, etc.).
    - clickup-format — final user-facing answer from tool results already gathered this turn; no new API calls.
      sub_message = how to frame the reply (e.g. "Answer with counts and task names" or "Markdown table of tasks").

    RULES:
    - agent_instance_id is always "default" for every step.
    - At least one clickup-execute step, then end with exactly one clickup-format step.
    - NEVER emit a plan whose only step is clickup-format — that is invalid. TypeScript coordination rejects it.
    - Multiple clickup-execute steps when separate tool passes are needed (e.g. list teams then list tasks).
    - Respect operation_kind: read vs write vs delete in sub_messages (delete flow needs confirmation semantics in executor).
    - If the intent already names a task_id or list_id, fold that into the first execute sub_message.
    - citations (optional): when the session transcript below is present, set citations to #N line refs; you may shorten later
      execute sub_messages when IDs are already in history, but you MUST still include ≥1 clickup-execute before clickup-format.

{{ ctx.output_format }}

Return exactly one `ClickUpStructuredPlan` JSON object.
Do not add text before or after the JSON object.
"#
}

/// Step executor: called repeatedly by runGeneratedStepExecutor for ONE plan step at a time.
/// Prior results are threaded forward so execute steps can use IDs found in navigate steps.
class FinalResponse {
  message string @description("Final answer or summary when this step is complete. For the 'format' step, prefer GitHub-flavored Markdown; navigate/execute completions may stay brief plain text.")
  structured_json string? @description("Optional strict JSON for UI/tools (e.g. task ids, list ids); omit when not needed.")
  citations string[] @description("URLs or stable refs from tool results; use [] if none.")
}

function ChooseClickUpAction(
  goal: string,
  step_description: string,
  operation_kind: string,
  prior_results: string?,
  session_context: SessionContext?,
) -> FinalResponse | SupportClickupSessionPlan {
  client DefaultClient
  prompt #"
    You are a ClickUp workspace assistant executing one plan step.
    Stay focused on this step only and use prior tool evidence from this turn.

    Overall goal: {{ goal }}
    Current step: {{ step_description }}
    Operation kind: {{ operation_kind }}

    Domain rules:
    - Reuse IDs from prior_results / history; do not invent IDs for required fields.
    - For read steps, gather only the data needed for this step and then complete.
    - For write steps, perform the requested change and then confirm what changed.
    - For delete operations: require explicit confirmation flow. First send confirm_delete=false; only send confirm_delete=true after clear user confirmation in prior results/history.
    - For final formatting steps, return FinalResponse with a user-ready message grounded in retrieved results.

    {% if prior_results %}
    Results from previous steps (use IDs and data from here):
    {{ prior_results }}
    {% endif %}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
