// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseCalcTool__select(
  user_message: string
) -> SupportCalculateOpenStep {

  client DefaultClient
  prompt #"The user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

/// Phase: act — send query to support/calculate.
function ChooseCalcTool__act__support_calculate(
  user_message: string
) -> SupportCalculateSendStep {

  client DefaultClient
  prompt #"The user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

/// Phase: continue — read, send again, or finish support/calculate.
function ChooseCalcTool__continue__support_calculate(
  user_message: string
) -> SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep {

  client DefaultClient
  prompt #"The user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

