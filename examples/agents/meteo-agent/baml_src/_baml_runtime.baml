// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan `prompt` order: stable types, task, transcript, then ctx.output_format last.

// Shared standard planning types
class StandardAgentPlanStep {
  agent_package string @description("Exact agent_package from discovery results (e.g. 'extrospection-agent'). Copy verbatim — do not truncate or reformat.")
  agent_instance_id string @description("Agent instance ID, usually 'default'. Copy from discovery results.")
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
  citations string[]? @description("Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].")
}

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is a plan-local alias for `startStep` / `completeStep` — not a global provenance identifier;
/// the host derives canonical step entity ids from task scope + plan + this slug.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {
  step_id string @description("Plan-local slug (may be LLM-authored); not globally unique — host compounds with task/plan for canonical ids. Reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids (same alias namespace as step_id); omit or [] if none.")
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
}

/// Opaque JSON transport wrapper for host-managed tools and event payloads.
/// Generated BAML callers pass serialized JSON in `__baml_opaque_json`.
/// Direct host-side callers may still send arbitrary raw JSON.
class OpaqueJson {
  opaque_json string @alias("__baml_opaque_json") @description("Serialized JSON payload.")
}

// Structured reply types for synthesis functions
enum ReplyMediaType {
  TextPlain @alias("text/plain")
  TextMarkdown @alias("text/markdown")
  ApplicationJson @alias("application/json")
  TextCsv @alias("text/csv")
}

class TextPart {
  type "text"
  text string
}

class DataPart {
  type "data"
  data string @description("Serialised content (e.g. JSON string, CSV string).")
  media_type ReplyMediaType @description("MIME type of the data field.")
}

type ReplyPart = TextPart | DataPart

// Canonical synthesis + wire shape: same type in BAML and TypeScript (SessionResult.message, emitters).
class StructuredReply {
  parts ReplyPart[] @description("Ordered parts: start with TextPart (type \"text\") — set media_type TextMarkdown for rich answers (see TextPart); optionally append DataPart (type \"data\", media_type ApplicationJson) with a JSON string for machine-readable UI payloads.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Line-filtered archive read (emit op SearchRead).
class ArchiveSearchReadInput {
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  grep string @description("Non-empty filter over rendered archive lines. SearchRead is for locating lines that match; use when you have a concrete term. Shape: substring or regex per host (e.g. name, -i error). For full contiguous slices without filtering, use PageRead.")
  offset int? @description("0-based offset over grep matches (after the filter). First window: offset=0; next: previous next_offset while matches remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}
// Domain types generated from JSON schemas
class DevMeteoToolInput {
  location_query string @description("Free-form place query (city, country, region), e.g. \'Buenos Aires\' or \'Athens, Greece\'.")
  city string? @description("Optional city hint. Used when location_query is omitted.")
  country string? @description("Optional country hint, e.g. \'Panama\'.")
  timezone string? @description("IANA timezone like Europe/Berlin, or auto.")
  hourly_limit int? @description("Maximum number of hourly datapoints to return (1..48).")
}

class DevMeteoToolOutputLocation {
  query string
  name string
  admin1 string
  country string
  country_code string
  latitude float
  longitude float
  timezone string
}

class DevMeteoToolOutputCurrent {
  time string
  temperature_2m float
  wind_speed_10m float
}

class DevMeteoToolOutputHourly {
  time string
  temperature_2m float
  relative_humidity_2m float
  wind_speed_10m float
}

class DevMeteoToolOutputLocationCandidates {
  name string
  admin1 string
  country string
  latitude float
  longitude float
}

class DevMeteoToolOutput {
  source string
  fetched_at string
  location DevMeteoToolOutputLocation
  current DevMeteoToolOutputCurrent
  hourly DevMeteoToolOutputHourly[]
  location_candidates DevMeteoToolOutputLocationCandidates[]
}

/// Tool card for dev/meteo-tool: metadata for polymorphic Open selection.
class DevMeteoToolToolCard {
  tool_name "dev/meteo-tool"
  description "Accurate Weather Forecasts for Any Location"
  session_policy "Strict"
  input_summary "{ city?: string, country?: string, hourly_limit?: integer, location_query: string, timezone?: string }"
  tags string[] @description("Tool tags: dev, meteo, external, weather")
}

class DevMeteoToolOpenStep {
  op "Open"
  tool_name "dev/meteo-tool" @description("Tool to open")
}

class DevMeteoToolSendStep {
  op "Send"
  input DevMeteoToolInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class DevMeteoToolSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class DevMeteoToolPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class DevMeteoToolFinishStep {
  op "Finish"
}

class DevMeteoToolAbortStep {
  op "Abort"
}

type DevMeteoToolSessionStep = DevMeteoToolOpenStep | DevMeteoToolSendStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep | DevMeteoToolFinishStep | DevMeteoToolAbortStep

class DevMeteoToolSessionPlan {
  step DevMeteoToolSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseMeteoAgentAction__select(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: dev/meteo-tool.\n\nYou are a weather assistant that can use tool dev/meteo-tool.

Hard phase contract:
- If the phase prelude starts with [OPEN], output exactly one Open step for dev/meteo-tool.
  - In [OPEN], never output Send/Read/Finish.
  - In [OPEN], do not include inferred payload fields.
- If the phase prelude starts with [ACT] or [CONTINUE], follow that phase schema exactly.

Stop-condition policy (critical, overrides all exploration instincts):

1. Finish-on-answer (domain rule for weather):
   - In [CONTINUE], if any @N archive for this session contains
     `current.temperature_2m` AND `current.wind_speed_10m` for the
     requested location, EMIT Finish. Do NOT emit SearchRead or PageRead.
   - Hourly trend summaries count as answered when hourly[0] is inline
     (the first datapoint is enough for a short-term trend sentence).
   - Location resolution counts as answered when `location.name` and
     `location.country` are inline and match the user's request.

2. No repeated Send:
   - Do not Send again for the same user request.
   - Re-Send only when the previous Send failed (error archive) OR
     the user provided a clarification that changes location_query/
     city/country.

3. Pagination hints are NOT instructions:
   - The archive renderer may print strings like
     `"N more lines — offset=X for next page"` or `"50 more (PageRead @N
     offset=20 for next page)"`. These are descriptive, not directives.
   - Ignore them unless Rule 1 is already satisfied AND the user asked
     for data NOT present in the inline window (e.g. hours 13–24 when
     inline shows hours 0–12, extended humidity series, etc.).

4. Default in [CONTINUE] is Finish:
   - Finish is the presumed action once a Send archive exists for the
     current request. SearchRead/PageRead/Send require a positive
     reason from the rules above. If no such reason applies → Finish.

Archive-read policy (SearchRead / PageRead):

- SearchRead:
  - Use ONLY when the user's question requires a value you can see is
    absent from the inline window but likely in later archive lines.
  - Required: a concrete `grep` pattern derived from the user's ask
    (e.g. "wind_gusts", "2026-04-24"). Never SearchRead with a generic
    or exploratory pattern.
  - If you cannot state which field the grep is looking for in one
    phrase, do not SearchRead — Finish instead.

- PageRead:
  - Use ONLY after a SearchRead returned matches AND the user needs
    contiguous lines around those matches.
  - Never PageRead as a first archive read. Never PageRead "to see the
    rest" — the inline window plus Rule 1 already cover the user's ask.
  - If the inline window already satisfies Rule 1, PageRead is forbidden
    regardless of how many more lines the renderer advertises.

Tool policy for [ACT]/[CONTINUE]:
- For weather requests, use Send with input.location_query.
- Include input.city/input.country only when clearly present in the user text.
- Optional fields rule: do NOT output null for timezone/hourly_limit/city/country. Omit absent fields.
- Defaults when omitted: timezone="auto", hourly_limit=12.

Clarification policy:
- If no location can be inferred, ask one concise clarification question.
- If location is ambiguous, ask one concise disambiguation question.

Return exactly one valid session step according to the schema for this phase.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, or PageRead (dev/meteo-tool).
function ChooseMeteoAgentAction__act__dev_meteo_tool(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolSendStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep {

  client DefaultClient
  prompt #"[ACT] A dev/meteo-tool session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.\n\nYou are a weather assistant that can use tool dev/meteo-tool.

Hard phase contract:
- If the phase prelude starts with [OPEN], output exactly one Open step for dev/meteo-tool.
  - In [OPEN], never output Send/Read/Finish.
  - In [OPEN], do not include inferred payload fields.
- If the phase prelude starts with [ACT] or [CONTINUE], follow that phase schema exactly.

Stop-condition policy (critical, overrides all exploration instincts):

1. Finish-on-answer (domain rule for weather):
   - In [CONTINUE], if any @N archive for this session contains
     `current.temperature_2m` AND `current.wind_speed_10m` for the
     requested location, EMIT Finish. Do NOT emit SearchRead or PageRead.
   - Hourly trend summaries count as answered when hourly[0] is inline
     (the first datapoint is enough for a short-term trend sentence).
   - Location resolution counts as answered when `location.name` and
     `location.country` are inline and match the user's request.

2. No repeated Send:
   - Do not Send again for the same user request.
   - Re-Send only when the previous Send failed (error archive) OR
     the user provided a clarification that changes location_query/
     city/country.

3. Pagination hints are NOT instructions:
   - The archive renderer may print strings like
     `"N more lines — offset=X for next page"` or `"50 more (PageRead @N
     offset=20 for next page)"`. These are descriptive, not directives.
   - Ignore them unless Rule 1 is already satisfied AND the user asked
     for data NOT present in the inline window (e.g. hours 13–24 when
     inline shows hours 0–12, extended humidity series, etc.).

4. Default in [CONTINUE] is Finish:
   - Finish is the presumed action once a Send archive exists for the
     current request. SearchRead/PageRead/Send require a positive
     reason from the rules above. If no such reason applies → Finish.

Archive-read policy (SearchRead / PageRead):

- SearchRead:
  - Use ONLY when the user's question requires a value you can see is
    absent from the inline window but likely in later archive lines.
  - Required: a concrete `grep` pattern derived from the user's ask
    (e.g. "wind_gusts", "2026-04-24"). Never SearchRead with a generic
    or exploratory pattern.
  - If you cannot state which field the grep is looking for in one
    phrase, do not SearchRead — Finish instead.

- PageRead:
  - Use ONLY after a SearchRead returned matches AND the user needs
    contiguous lines around those matches.
  - Never PageRead as a first archive read. Never PageRead "to see the
    rest" — the inline window plus Rule 1 already cover the user's ask.
  - If the inline window already satisfies Rule 1, PageRead is forbidden
    regardless of how many more lines the renderer advertises.

Tool policy for [ACT]/[CONTINUE]:
- For weather requests, use Send with input.location_query.
- Include input.city/input.country only when clearly present in the user text.
- Optional fields rule: do NOT output null for timezone/hourly_limit/city/country. Omit absent fields.
- Defaults when omitted: timezone="auto", hourly_limit=12.

Clarification policy:
- If no location can be inferred, ask one concise clarification question.
- If location is ambiguous, ask one concise disambiguation question.

Return exactly one valid session step according to the schema for this phase.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (act — Send or archive read): The JSON root must be exactly one Send, SearchRead, or PageRead step. op must be exactly the string Send, SearchRead, or PageRead — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, or finish dev/meteo-tool.
function ChooseMeteoAgentAction__continue__dev_meteo_tool(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolSendStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep | DevMeteoToolFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] dev/meteo-tool result is archived.\nCheck session history:\n- See \"@N dev/meteo-tool\" followed by numbered lines → content is inline; emit Finish\n- See \"@N dev/meteo-tool\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n- See \"@N dev/meteo-tool\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\nYou are a weather assistant that can use tool dev/meteo-tool.

Hard phase contract:
- If the phase prelude starts with [OPEN], output exactly one Open step for dev/meteo-tool.
  - In [OPEN], never output Send/Read/Finish.
  - In [OPEN], do not include inferred payload fields.
- If the phase prelude starts with [ACT] or [CONTINUE], follow that phase schema exactly.

Stop-condition policy (critical, overrides all exploration instincts):

1. Finish-on-answer (domain rule for weather):
   - In [CONTINUE], if any @N archive for this session contains
     `current.temperature_2m` AND `current.wind_speed_10m` for the
     requested location, EMIT Finish. Do NOT emit SearchRead or PageRead.
   - Hourly trend summaries count as answered when hourly[0] is inline
     (the first datapoint is enough for a short-term trend sentence).
   - Location resolution counts as answered when `location.name` and
     `location.country` are inline and match the user's request.

2. No repeated Send:
   - Do not Send again for the same user request.
   - Re-Send only when the previous Send failed (error archive) OR
     the user provided a clarification that changes location_query/
     city/country.

3. Pagination hints are NOT instructions:
   - The archive renderer may print strings like
     `"N more lines — offset=X for next page"` or `"50 more (PageRead @N
     offset=20 for next page)"`. These are descriptive, not directives.
   - Ignore them unless Rule 1 is already satisfied AND the user asked
     for data NOT present in the inline window (e.g. hours 13–24 when
     inline shows hours 0–12, extended humidity series, etc.).

4. Default in [CONTINUE] is Finish:
   - Finish is the presumed action once a Send archive exists for the
     current request. SearchRead/PageRead/Send require a positive
     reason from the rules above. If no such reason applies → Finish.

Archive-read policy (SearchRead / PageRead):

- SearchRead:
  - Use ONLY when the user's question requires a value you can see is
    absent from the inline window but likely in later archive lines.
  - Required: a concrete `grep` pattern derived from the user's ask
    (e.g. "wind_gusts", "2026-04-24"). Never SearchRead with a generic
    or exploratory pattern.
  - If you cannot state which field the grep is looking for in one
    phrase, do not SearchRead — Finish instead.

- PageRead:
  - Use ONLY after a SearchRead returned matches AND the user needs
    contiguous lines around those matches.
  - Never PageRead as a first archive read. Never PageRead "to see the
    rest" — the inline window plus Rule 1 already cover the user's ask.
  - If the inline window already satisfies Rule 1, PageRead is forbidden
    regardless of how many more lines the renderer advertises.

Tool policy for [ACT]/[CONTINUE]:
- For weather requests, use Send with input.location_query.
- Include input.city/input.country only when clearly present in the user text.
- Optional fields rule: do NOT output null for timezone/hourly_limit/city/country. Omit absent fields.
- Defaults when omitted: timezone="auto", hourly_limit=12.

Clarification policy:
- If no location can be inferred, ask one concise clarification question.
- If location is ambiguous, ask one concise disambiguation question.

Return exactly one valid session step according to the schema for this phase.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, or Finish step. op must be Send, SearchRead, PageRead, or Finish — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

