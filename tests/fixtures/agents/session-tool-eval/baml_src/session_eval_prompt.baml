function PlanSyntheticSessionStep(
  objective: string,
  session_context: SessionContext,
  session_state: SyntheticPlannerState?,
  output_summary: string?,
) -> SyntheticPlannerPlan {
  client DefaultClient
  prompt #"
    Output ONLY one JSON object with exactly one session `decision`.
    No prose. No markdown.
    Read required operation from session_context.allowed_ops (authoritative).
    Emit exactly one decision whose op is one of allowed_ops.
    If allowed_ops has a single entry, emit that op exactly.

    Objective:
    1) reach goal_id = non_trivial_scope_cache_goal
    2) minimize wasted reads and payload
    3) use projection progression: summary -> identity -> detail before Finish

    Rules:
    - Open: optional reason only.
    - Send: set/refresh scope defaults.
    - Read: include input.read.refId; projection MUST be one of summary|identity|detail.
    - If session_state is null and Read is allowed, use read.refId = "root" with projection "summary".
    - If Read is allowed and session_state.candidates is non-empty, pick max severity ref_id.
    - If Read is allowed and candidates empty but refs non-empty, use refs[0].
    - Finish only when session_state.goal_id == "non_trivial_scope_cache_goal"
      and session_state.projection_progress.used includes summary, identity, and detail.
    - Do not emit extra keys outside schema.

    Current objective: {{ objective }}
    Runtime state:
    - session_context: {{ session_context }}
    - session_state: {{ session_state }}
    {{ ctx.output_format }}
  "#
}

function SummarizeSyntheticSessionOutput(
  objective: string,
  tool_output_json: string,
  used_projections: string[],
) -> SyntheticPlannerState {
  client DefaultClient
  prompt #"
    Output ONLY one JSON object. No prose. No markdown.

    Build SyntheticPlannerState from tool_output_json.
    - Parse tool_output_json as JSON.
    - level = output.level (default "unknown")
    - goal_id = output.goal_id or null
    - refs = output.refs as string[] (default [])
    - candidates: derive from output.rows by:
      failed_count = row.failed_count
      uncached_prompt_tokens = row.prompt_tokens_total - row.cached_input_tokens_total
      severity = (failed_count * 100) + uncached_prompt_tokens
    - projection_progress.used = used_projections (lowercase)
    - projection_progress.required = ["summary","identity","detail"]
    - last_read_ref = first ref in refs when present, else null

    Current objective: {{ objective }}
    Inputs:
    - tool_output_json: {{ tool_output_json }}
    - used_projections: {{ used_projections }}
    {{ ctx.output_format }}
  "#
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model "x-ai/grok-4.1-fast"
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
