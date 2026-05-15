// Unified step-executor harness: `ChooseSlackAction` is a *SessionPlan root — `runGeneratedStepExecutor`
// shares prefix algebra, transcript-only tags, and intra-turn history with unified-primary roots; this path
// emits the full tool FSM (`__entry` / `__active__*`). Planner-only unions use `unified_step_executors.json`.

/// Phase 1 — Intent: classify Slack-related requests; clarify; reject unrelated chat.
class NeedClarification {
  kind "clarify"
  question string @description("One concrete question when scope, channel, thread, or search target is missing.")
}

class NotRelevant {
  kind "not_relevant"
  reason string @description("Brief reason this is not a read-only Slack workspace question.")
}

class SlackIntent {
  kind "intent"
  intent string @description("Distilled goal: what to read or search for in Slack (no execution details).")
}

/// One row in the Slack host plan — wire shape matches coordination `StandardAgentPlanStep` (same JSON keys).
class SlackPlanStep {
  agent_package string @description("Exactly slack-retrieve (one read-only Slack tool pass: list conversations, history, thread replies, search, resolve users) or slack-synthesize (final user answer from session evidence only; no new API calls). No other sentinel values.")
  agent_instance_id string @description("Always the literal default for this host: default")
  sub_message string @description("slack-retrieve: concrete executor instruction (channel_id, limits, query, etc.). For GetThreadRepliesInput, thread_ts MUST be Slack Web API form <unix_seconds>.<six_fractional_digits> (e.g. 1735689600.000000). A permalink segment like p1735689600000000 is NOT thread_ts — derive the API ts before planning. slack-synthesize: how to frame the final reply (format, citations).")
}

/// Slack host structured plan — top-level keys align with shared StandardStructuredPlan for coordination code.
class SlackStructuredPlan {
  intent_description string @description("One sentence: user-visible outcome (not low-level API mechanics).")
  objective string @description("One sentence: what this turn achieves (may echo intent).")
  plan_steps SlackPlanStep[] @description("Ordered steps: at least one slack-retrieve, then exactly one slack-synthesize as the LAST step. Extra slack-retrieve rows only when separate API passes are needed. Never plan writes, posts, edits, or DMs.")
  citations string[]? @description("Optional #N / @N line refs for observability when grounded in history; otherwise omit or [].")
}

function InferSlackIntent(user_message: string) -> NeedClarification | NotRelevant | SlackIntent {
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
Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You classify messages for a read-only Slack assistant (history, threads, search, summaries, action items).
    Do NOT call tools or invent channel IDs — only classify.

    Treat the request as a real Slack intent when it is achievable with read-only Slack APIs
    (conversations, messages, search, user resolution).
    Ask one clarifying question when it is Slack-related but missing essentials
    (which channel, thread link, search query, time range, etc.).
    Mark it not relevant only when it has nothing to do with Slack or workspace messages.

    If history shows you already asked a clarifying question and the user replied vaguely, reuse earlier context
    and prefer a concrete Slack goal when a reasonable read-only interpretation exists.

    {{ _.role('user') }}
    {{ user_message }}

{{ ctx.output_format }}

Return exactly one JSON object.
Select the object shape with discriminator `kind`:
- `kind: "clarify"` -> NeedClarification
- `kind: "not_relevant"` -> NotRelevant
- `kind: "intent"` -> SlackIntent
Set `kind` exactly. Do not mix fields from different object shapes.
Do not add text before or after the JSON object.
"#
}

/// Phase 2 — Plan read-only Slack work. Contract is entirely in `SlackStructuredPlan` / `SlackPlanStep` field descriptions and `{{ ctx.output_format }}` — do not duplicate schema rules in prose here.
function PlanSlackWork(intent: string) -> SlackStructuredPlan {
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
Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

Plan read-only Slack work for this validated intent. Follow the output schema field descriptions exactly.
    {{ intent }}

{{ ctx.output_format }}

Return exactly one `SlackStructuredPlan` JSON object.
Do not add text before or after the JSON object.
"#
}

class ReadOnlyResponse {
  message string @description("Status or short excerpt when this retrieve step is complete; Markdown ok.")
  next_step string?
}

/// Per retrieve-step executor: drives support/slack Open → Send → Read → Finish.
/// Prior retrieve steps on this turn appear in generated phase session history — no separate prior_results blob.
function ChooseSlackAction(
  goal: string,
  step_description: string,
) -> ReadOnlyResponse | SupportSlackSessionPlan {
  client DefaultClient
  prompt #"
    You execute ONE retrieve step of a read-only Slack plan.
    Overall goal: {{ goal }}
    This step: {{ step_description }}

    When emitting a session step: respect session history for channel_ids, cursors, and `thread_ts` (Slack API form: <unix>.<frac>; not permalink `p…` forms).
    When this retrieve step is complete without further tool work, return ReadOnlyResponse instead of a session step.
  "#
}

/// Final user-facing reply from retrieved Slack evidence (session history carries tool results).
function ReactToSlackResults(
  goal: string,
  user_message: string,
) -> StructuredReply {
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
Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You answer using read-only Slack data already in the session history (tool results, archives).
    Match the output schema exactly (StructuredReply: parts + citations).

    User message: {{ user_message }}
    Goal: {{ goal }}

    Be faithful to retrieved content; if data is missing, say what was not found and what the user should provide.

{{ ctx.output_format }}

Return exactly one `StructuredReply` JSON object.
If `parts[]` is present, choose each item with discriminator `type`:
- `type: "text"` -> TextPart
- `type: "data"` -> DataPart
Set `type` exactly for each `parts[]` item.
Do not add text before or after the JSON object.
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
