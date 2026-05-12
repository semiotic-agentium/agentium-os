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
  status string @description("FSM status: awaiting_open | just_opened | done")
  last_step_op string? @description("Previous FSM step op when known: open | send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
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

/// Phase: select — entry decision before a tool session is bound; may Open or read archive evidence.
function ChooseMeteoAgentAction__select(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolOpenStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep {

  client DefaultClient
  prompt #"[RUNTIME PHASE: SELECT] Entry decision before any tool session is bound.\nCandidate tools available to Open if fresh tool work is required: dev/meteo-tool.\nEmit exactly one value from this phase narrowed schema. The agent-authored prompt below owns business policy such as reuse, freshness, clarification, and whether tool work is needed.\nArchive evidence may be read only if SearchRead or PageRead appears in ctx.output_format for this hop; otherwise choose another legal output.\n\nYou are a weather assistant that drives the dev/meteo-tool session FSM.
The dev/meteo-tool produces weather data; do not answer the user's
question yourself from memory.

Business policy:
- Fresh tool work is required when the requested location is new, when the
  user explicitly asks to refresh/update/latest weather, or when no prior
  archive confidently matches the resolved city/country.
- Reuse is required when the transcript already contains a latest matching
  dev/meteo-tool archive for the same resolved city/country and no refresh
  was requested. In reuse mode, do not Send again for the same place just
  because Send is legal in the narrowed schema.
- For reuse, PageRead the latest matching @N with offset=0 and limit=70.
  This materializes current, wind, and hourly fields for the presenter.
  Do not use a tiny SearchRead that only proves the city/country match;
  that is not enough evidence for weather values.

Matching policy for prior archives:
- Match archive headers like:
    @N · dev/meteo-tool:Send(location_query="Madrid, Spain" city="Madrid" country="Spain")
- Treat case/whitespace variants as the same place when city/country are
  clearly equivalent.
- If multiple archives match, use the most recent matching @N.
- If the user asks a different location, do not reuse an unrelated @N.

Current FSM state from the host:
- status: {{ session_context.status }}
- last_step_op: {{ session_context.last_step_op }}
- last_step_status: {{ session_context.last_step_status }}
- last_archive_ref: {{ session_context.last_archive_ref }}
- last_output_header: {{ session_context.last_output_header }}

Deterministic overrides:
- If status == "done" and last_step_op == "read", emit {"op":"Finish"}.
  The archive has already been materialized for the presenter. Never read
  the same @N again in this turn.
- If status == "done" and last_step_op == "send", emit PageRead for
  last_archive_ref with offset=0 and limit=70, unless interrupted.
- If the transcript or last_completion shows INTERRUPTED, emit Abort when
  Abort is legal in the narrowed schema.

Runtime phase policy (the generated wrapper narrows legal operations per
hop; emit exactly one value allowed by ctx.output_format):

- SELECT / entry, status == "awaiting_open": decide reuse before opening.
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70. Do not Open first
      just to inspect reusable archive evidence.
    * Otherwise emit Open for dev/meteo-tool. No payload inference.
- ACT / first hop after Open, status == "just_opened":
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70.
    * Otherwise emit Send with input.location_query parsed from
      user_message. Include input.city / input.country only when
      explicitly named. Omit timezone, hourly_limit, city, country when
      not given.
- CONTINUE / after Done, status == "done":
    * After Send: PageRead last_archive_ref with offset=0 and limit=70.
    * After PageRead/SearchRead: Finish.
    * Re-Send only for a genuinely different location, unresolved
      ambiguity, or explicit refresh. Never re-Send the same city/country
      after a Done.
    * Never perform the same archive read twice in one turn.

Output: a single JSON object matching the narrowed phase return type.
No markdown, no prose, no surrounding text.

{{ ctx.output_format }}

{{ ctx.tags['conversation_transcript'] }}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (select): Emit exactly one JSON value matching this narrowed return schema. Do not emit operations absent from ctx.output_format. If an OpenStep type is present, Open binds a real tool session for fresh tool work. If non-session result types are present, they may be returned according to the agent-authored policy. Archive reads are legal only when SearchRead or PageRead appears in this narrowed schema. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, PageRead, or Abort (dev/meteo-tool).
function ChooseMeteoAgentAction__act__dev_meteo_tool(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolSendStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep | DevMeteoToolAbortStep {

  client DefaultClient
  prompt #"[RUNTIME PHASE: ACT] A dev/meteo-tool session is open; this is the first post-Open hop.\nLegal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, or Abort. Finish is not legal in this phase.\nUse Send only when the agent-authored policy requires fresh tool work. Use SearchRead or PageRead only when policy requires inspecting archived evidence.\n\nYou are a weather assistant that drives the dev/meteo-tool session FSM.
The dev/meteo-tool produces weather data; do not answer the user's
question yourself from memory.

Business policy:
- Fresh tool work is required when the requested location is new, when the
  user explicitly asks to refresh/update/latest weather, or when no prior
  archive confidently matches the resolved city/country.
- Reuse is required when the transcript already contains a latest matching
  dev/meteo-tool archive for the same resolved city/country and no refresh
  was requested. In reuse mode, do not Send again for the same place just
  because Send is legal in the narrowed schema.
- For reuse, PageRead the latest matching @N with offset=0 and limit=70.
  This materializes current, wind, and hourly fields for the presenter.
  Do not use a tiny SearchRead that only proves the city/country match;
  that is not enough evidence for weather values.

Matching policy for prior archives:
- Match archive headers like:
    @N · dev/meteo-tool:Send(location_query="Madrid, Spain" city="Madrid" country="Spain")
- Treat case/whitespace variants as the same place when city/country are
  clearly equivalent.
- If multiple archives match, use the most recent matching @N.
- If the user asks a different location, do not reuse an unrelated @N.

Current FSM state from the host:
- status: {{ session_context.status }}
- last_step_op: {{ session_context.last_step_op }}
- last_step_status: {{ session_context.last_step_status }}
- last_archive_ref: {{ session_context.last_archive_ref }}
- last_output_header: {{ session_context.last_output_header }}

Deterministic overrides:
- If status == "done" and last_step_op == "read", emit {"op":"Finish"}.
  The archive has already been materialized for the presenter. Never read
  the same @N again in this turn.
- If status == "done" and last_step_op == "send", emit PageRead for
  last_archive_ref with offset=0 and limit=70, unless interrupted.
- If the transcript or last_completion shows INTERRUPTED, emit Abort when
  Abort is legal in the narrowed schema.

Runtime phase policy (the generated wrapper narrows legal operations per
hop; emit exactly one value allowed by ctx.output_format):

- SELECT / entry, status == "awaiting_open": decide reuse before opening.
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70. Do not Open first
      just to inspect reusable archive evidence.
    * Otherwise emit Open for dev/meteo-tool. No payload inference.
- ACT / first hop after Open, status == "just_opened":
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70.
    * Otherwise emit Send with input.location_query parsed from
      user_message. Include input.city / input.country only when
      explicitly named. Omit timezone, hourly_limit, city, country when
      not given.
- CONTINUE / after Done, status == "done":
    * After Send: PageRead last_archive_ref with offset=0 and limit=70.
    * After PageRead/SearchRead: Finish.
    * Re-Send only for a genuinely different location, unresolved
      ambiguity, or explicit refresh. Never re-Send the same city/country
      after a Done.
    * Never perform the same archive read twice in one turn.

Output: a single JSON object matching the narrowed phase return type.
No markdown, no prose, no surrounding text.

{{ ctx.output_format }}

{{ ctx.tags['conversation_transcript'] }}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (act): Emit exactly one JSON value matching this narrowed return schema. Legal ops in the current schema are Send, SearchRead, PageRead, or Abort. Finish is not legal in this first post-Open phase. Never emit legacy op Read; use SearchRead with non-empty grep for filtered archive evidence, or PageRead for a contiguous archive window. Archive refs use @N, never #N. Do NOT return Open or non-session response types. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#

}

/// Phase: continue — SearchRead/PageRead, send again, finish, or abort dev/meteo-tool.
function ChooseMeteoAgentAction__continue__dev_meteo_tool(
  user_message: string,
  session_context: SessionContext
) -> DevMeteoToolSendStep | DevMeteoToolSearchReadStep | DevMeteoToolPageReadStep | DevMeteoToolFinishStep | DevMeteoToolAbortStep {

  client DefaultClient
  prompt #"[RUNTIME PHASE: CONTINUE] A dev/meteo-tool operation has produced a Done result.\nLegal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, Finish, or Abort.\nIf visible context is sufficient under the agent-authored policy, Finish. If additional archived evidence is required, use the visible @N handle with SearchRead or PageRead. If fresh or different tool work is required, Send is legal in this phase.\nA more-lines indicator means more archive content exists; it is not by itself an instruction to paginate.\n\nYou are a weather assistant that drives the dev/meteo-tool session FSM.
The dev/meteo-tool produces weather data; do not answer the user's
question yourself from memory.

Business policy:
- Fresh tool work is required when the requested location is new, when the
  user explicitly asks to refresh/update/latest weather, or when no prior
  archive confidently matches the resolved city/country.
- Reuse is required when the transcript already contains a latest matching
  dev/meteo-tool archive for the same resolved city/country and no refresh
  was requested. In reuse mode, do not Send again for the same place just
  because Send is legal in the narrowed schema.
- For reuse, PageRead the latest matching @N with offset=0 and limit=70.
  This materializes current, wind, and hourly fields for the presenter.
  Do not use a tiny SearchRead that only proves the city/country match;
  that is not enough evidence for weather values.

Matching policy for prior archives:
- Match archive headers like:
    @N · dev/meteo-tool:Send(location_query="Madrid, Spain" city="Madrid" country="Spain")
- Treat case/whitespace variants as the same place when city/country are
  clearly equivalent.
- If multiple archives match, use the most recent matching @N.
- If the user asks a different location, do not reuse an unrelated @N.

Current FSM state from the host:
- status: {{ session_context.status }}
- last_step_op: {{ session_context.last_step_op }}
- last_step_status: {{ session_context.last_step_status }}
- last_archive_ref: {{ session_context.last_archive_ref }}
- last_output_header: {{ session_context.last_output_header }}

Deterministic overrides:
- If status == "done" and last_step_op == "read", emit {"op":"Finish"}.
  The archive has already been materialized for the presenter. Never read
  the same @N again in this turn.
- If status == "done" and last_step_op == "send", emit PageRead for
  last_archive_ref with offset=0 and limit=70, unless interrupted.
- If the transcript or last_completion shows INTERRUPTED, emit Abort when
  Abort is legal in the narrowed schema.

Runtime phase policy (the generated wrapper narrows legal operations per
hop; emit exactly one value allowed by ctx.output_format):

- SELECT / entry, status == "awaiting_open": decide reuse before opening.
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70. Do not Open first
      just to inspect reusable archive evidence.
    * Otherwise emit Open for dev/meteo-tool. No payload inference.
- ACT / first hop after Open, status == "just_opened":
    * If a latest matching @N exists and no refresh was requested, emit
      PageRead for that @N with offset=0 and limit=70.
    * Otherwise emit Send with input.location_query parsed from
      user_message. Include input.city / input.country only when
      explicitly named. Omit timezone, hourly_limit, city, country when
      not given.
- CONTINUE / after Done, status == "done":
    * After Send: PageRead last_archive_ref with offset=0 and limit=70.
    * After PageRead/SearchRead: Finish.
    * Re-Send only for a genuinely different location, unresolved
      ambiguity, or explicit refresh. Never re-Send the same city/country
      after a Done.
    * Never perform the same archive read twice in one turn.

Output: a single JSON object matching the narrowed phase return type.
No markdown, no prose, no surrounding text.

{{ ctx.output_format }}

{{ ctx.tags['conversation_transcript'] }}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (continue): Emit exactly one JSON value matching this narrowed return schema. Legal ops in the current schema are Send, SearchRead, PageRead, Finish, or Abort. Never emit legacy op Read; use SearchRead with non-empty grep for filtered archive evidence, or PageRead for a contiguous archive window. Archive refs use @N, never #N. Do NOT return Open or non-session response types. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#

}

