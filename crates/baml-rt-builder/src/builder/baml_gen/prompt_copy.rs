//! Single source for repeated BAML `@description` text (citations, Read/archive policy).
//! [`render_generated_tools_prelude`] emits the shared prelude; [`tool_interfaces`] uses the same
//! citation/send/read strings for per-tool session classes.
//!
//! ## Prompt style (generated `@description` and FSM comments)
//!
//! - **Voice:** Imperative, second person implied. No hedging for host-enforced behaviour.
//! - **Strength:** **Must** / **Do not** — schema and namespace rules. **Use** / **Set** — operational
//!   defaults (e.g. large-archive Read: set `grep`, small `limit`, `offset`). **Optional** — fields
//!   the host allows empty (`[]`, omit).
//! - **Verbs:** **Emit** — one FSM step or session-plan fragment. **Return** — reserved for human
//!   doc / coordination intro (“return a report”) where it matches BAML wording; step rules use *emit*.
//! - **Refs:** Preserve `#N` vs `@N` and `!` exactly — see `docs/how-to-write-agents.md` §6.

use super::escape::escape_baml_description;

// --- Citations (#N / @N / line suffixes) ---

/// `StructuredReply.citations` and aligned session-plan decision citations (same ref rules).
pub(crate) const CITATIONS_DECISION_OR_SYNTHESIS: &str = "Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.";

/// `StandardStructuredPlan.citations` (optional on plans).
pub(crate) const CITATIONS_PLAN_OPTIONAL: &str = "Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].";

/// `*SendStep.citations`
pub(crate) const CITATIONS_SEND_STEP: &str = "Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.";

// --- Archive Read (grep-first, bounded windows) ---

pub(crate) const ARCHIVE_READ_ARCHIVE_REF: &str = "Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every Read must use a real @N; do not invent refs.";

pub(crate) const ARCHIVE_READ_OFFSET: &str = "0-based line offset after grep. Page with limit: first window offset=0; next offset += previous limit while lines or matches remain.";

pub(crate) const ARCHIVE_READ_LIMIT: &str = "Max lines in this Read window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded). Set grep before large windows.";

pub(crate) const ARCHIVE_READ_GREP: &str = "Large @N: set this first — line filter (substring or regex per host), e.g. deploy or -i deploy. Then page with offset/limit. Do not issue repeated wide Reads without a pattern.";

// --- Session plan `step` field guidance ---

pub(crate) const STEP_DESC_CLAUDE_OR_A2A: &str = "Emit one FSM step. From history: no session → Open; session open → Send (input.text must be non-empty) for new work, or Read @N when that tool archive already exists in history; after Send (@N archived) → Read @N (large archive: set grep, small limit, offset to page), Finish, or Send again.";

pub(crate) const STEP_DESC_DEFAULT: &str = "Emit one FSM step. From history: no session → Open; session open → Send for new work or Read @N when the archive already exists in history; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again.";

/// `*ReadStep` wrapper around `ArchiveReadInput` (field-level hint; full policy on ArchiveReadInput members).
pub(crate) const READ_STEP_INPUT_DESCRIPTION: &str = "archive_ref required (@N from a Send for this tool, including earlier history). Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.";

/// Full shared prelude (FSM header, planning types, StructuredReply, ArchiveReadInput).
pub fn render_generated_tools_prelude() -> String {
    let esc = escape_baml_description;
    let c_plan = esc(CITATIONS_PLAN_OPTIONAL);
    let c_reply = esc(CITATIONS_DECISION_OR_SYNTHESIS);
    let c_ar = esc(ARCHIVE_READ_ARCHIVE_REF);
    let c_off = esc(ARCHIVE_READ_OFFSET);
    let c_lim = esc(ARCHIVE_READ_LIMIT);
    let c_grep = esc(ARCHIVE_READ_GREP);
    format!(
        r#"// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: archive_ref @N from a Send for this tool (same session or conversation history). Large body: set grep; small limit; offset to page.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read needs a real @N from a Send for this tool (may be earlier in history). Large @N: grep first, then limit/offset — do not dump whole archives.
// - Always Finish or Abort to close the session

// Shared standard planning types
class StandardAgentPlanStep {{
  agent_package string @description("Exact agent_package from discovery results (e.g. 'extrospection-agent'). Copy verbatim — do not truncate or reformat.")
  agent_instance_id string @description("Agent instance ID, usually 'default'. Copy from discovery results.")
  sub_message string
}}

class StandardStructuredPlan {{
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
  citations string[]? @description("{c_plan}")
}}

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is a plan-local alias for `startStep` / `completeStep` — not a global provenance identifier;
/// the host derives canonical step entity ids from task scope + plan + this slug.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {{
  step_id string @description("Plan-local slug (may be LLM-authored); not globally unique — host compounds with task/plan for canonical ids. Reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids (same alias namespace as step_id); omit or [] if none.")
}}

class HistoryContext {{
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {{
  contract_version string
  session_open bool
  allowed_ops string[]? @description("Deprecated: per-phase step-executor functions narrow the output schema. Host may omit.")
  selected_tool string?
  status_token string?
}}

/// Opaque JSON transport wrapper for host-managed tools and event payloads.
/// Generated BAML callers pass serialized JSON in `__baml_opaque_json`.
/// Direct host-side callers may still send arbitrary raw JSON.
class OpaqueJson {{
  opaque_json string @alias("__baml_opaque_json") @description("Serialized JSON payload.")
}}

// Structured reply types for synthesis functions
enum ReplyMediaType {{
  TextPlain @alias("text/plain")
  TextMarkdown @alias("text/markdown")
  ApplicationJson @alias("application/json")
  TextCsv @alias("text/csv")
}}

class TextPart {{
  type "text"
  text string
}}

class DataPart {{
  type "data"
  data string @description("Serialised content (e.g. JSON string, CSV string).")
  media_type ReplyMediaType @description("MIME type of the data field.")
}}

type ReplyPart = TextPart | DataPart

// Canonical synthesis + wire shape: same type in BAML and TypeScript (SessionResult.message, emitters).
class StructuredReply {{
  parts ReplyPart[] @description("Ordered parts: start with TextPart (type \"text\") — set media_type TextMarkdown for rich answers (see TextPart); optionally append DataPart (type \"data\", media_type ApplicationJson) with a JSON string for machine-readable UI payloads.")
  citations string[] @description("{c_reply}")
}}

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {{
  archive_ref string @description("{c_ar}")
  offset int? @description("{c_off}")
  limit int? @description("{c_lim}")
  grep string? @description("{c_grep}")
}}
"#,
        c_plan = c_plan,
        c_reply = c_reply,
        c_ar = c_ar,
        c_off = c_off,
        c_lim = c_lim,
        c_grep = c_grep,
    )
}
