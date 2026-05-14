/// One-hop planner for the session-tool-eval loop: returns a **standard** structured plan
/// with exactly one step. The step’s `sub_message` holds a JSON line describing the next
/// tool-session op (Open / Send / SearchRead / PageRead / Finish) for `test/synthetic_session_eval`.
function PlanSessionToolEvalStep(
  objective: string,
) -> StandardStructuredPlan {
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
Output ONLY one JSON object matching StandardStructuredPlan. No prose. No markdown.

    User objective (seed): {{ objective }}

    Fill the schema as follows:
    - intent_description: one sentence — the overall eval goal.
    - objective: one sentence — what this single hop should accomplish.
    - plan_steps: **exactly one** StandardAgentPlanStep, the next operation only.

    That step MUST use:
      agent_package: "test/synthetic_session_eval"
      agent_instance_id: "default"
      sub_message: a single-line JSON string (valid JSON object) with:
        - "op": one of "Open", "Send", "SearchRead", "PageRead", "Finish"
        - optional "initial_input": object (use on Open when needed)
        - optional "input": object (Send / SearchRead / PageRead payloads)

    Example sub_message values:
      {"op":"Open","initial_input":{}}
      {"op":"Send","input":{"retrieve_ref":null}}
      {"op":"PageRead","input":{"retrieve_ref":"ref:1"}}
      {"op":"Finish"}

    Strategy (reflect in your fields and JSON):
    - Reach goal_id = non_trivial_scope_cache_goal via explicit session reads.
    - Use projection progression: summary → identity → detail before Finish.
    - Minimise wasted reads: once you have data at one projection level, advance to the next.

    The allowed ops and current session state are implied by session history below.
    Read the history to determine what has already been done, then emit the single next step.

{{ ctx.output_format }}

Return exactly one `StandardStructuredPlan` JSON object.
Do not add text before or after the JSON object.
"#
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

retry_policy ParseRetry {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 200
  }
}
