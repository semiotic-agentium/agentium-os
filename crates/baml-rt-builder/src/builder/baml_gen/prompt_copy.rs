//! Single source for repeated BAML `@description` text (citations, SearchRead/PageRead archive policy).
//! [`render_generated_tools_prelude`] emits the shared prelude; [`tool_interfaces`] uses the same
//! citation/send/read strings for per-tool session classes.
//!
//! ## Prompt style (generated `@description` and FSM comments)
//!
//! - **Voice:** Imperative, second person implied. No hedging for host-enforced behaviour.
//! - **Strength:** **Must** / **Do not** — schema and namespace rules. **Use** / **Set** — operational
//!   defaults. Read-tactics for windowed tool output are stated on the FSM step fields and in the
//!   in-context `offset=` / pagination line in `conversation_history`, not in the static prelude. **Optional** — fields
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

// --- Archive access (SearchRead = filter, PageRead = contiguous paging) ---

pub(crate) const ARCHIVE_READ_ARCHIVE_REF: &str = "Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.";

pub(crate) const ARCHIVE_SEARCH_READ_GREP: &str = "Non-empty line filter (substring or regex per host), e.g. name or -i error. Prefer SearchRead to narrow large or windowed archives before long PageRead scans. For unfiltered contiguous slices, use PageRead, not SearchRead.";

pub(crate) const ARCHIVE_SEARCH_READ_OFFSET: &str = "0-based offset counting lines that matched grep (after filter). Page with limit: first window offset=0; next offset = previous next_offset while matches remain.";

pub(crate) const ARCHIVE_PAGE_READ_OFFSET: &str = "0-based offset over full rendered archive lines (no grep). Page with limit: first window offset=0; next offset = previous next_offset while lines remain.";

pub(crate) const ARCHIVE_READ_LIMIT: &str = "Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).";

// --- Conversation history (ctx.tags) — must match runtime projection ---

/// Canonical Jinja for `ctx.tags['conversation_history']`: rows are `{ role, content }` from
/// [`baml_rt_tools::prompt_projection`], with optional `citations: string[]` on **message**-sourced
/// rows (wire ref-table strings); `content` lines already carry `#N` / tool-call refs in the text.
/// Same shape as `PersonaChat` in `tests/fixtures/agents/conversational-persona-demo`: `_.role` +
/// `content` on separate lines, loop variable `message`, no section header — do **not** use
/// `{{ msg.role }}: {{ msg.content }}` on one line.
///
/// Per-phase step executors copy the parent function's prompt verbatim from IR; this block is **not**
/// injected by codegen — use it (or equivalent) in hand-written session-plan `*_prompt.baml` files.
#[allow(dead_code)] // Authoring reference; kept for consistency with persona fixture prompts.
pub(crate) const BAML_CONVERSATION_HISTORY_JINJA_BLOCK: &str = r#"{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}
"#;

// --- Session plan `step` field guidance ---

pub(crate) const STEP_DESC_CLAUDE_OR_A2A: &str = "Emit one FSM step. From history: no session → Open; session open → Send (input.text must be non-empty) for new work, or SearchRead/PageRead @N when that tool’s archive already exists; after Send (@N archived) → SearchRead (non-empty grep) to find lines, PageRead for contiguous unfiltered text, re-Send, or Finish. When history shows a partial or windowed @N, emit SearchRead or PageRead (offsets in the host line in conversation) before Finish if the task still needs unread lines.";

pub(crate) const STEP_DESC_DEFAULT: &str = "Emit one FSM step. From history: no session → Open; session open → Send with the tool’s input for new work, or SearchRead/PageRead @N when that tool’s archive already exists; after Send (@N archived) → SearchRead (non-empty grep) to find lines, PageRead for contiguous unfiltered text, re-Send, or Finish. When history shows a partial or windowed @N, emit SearchRead or PageRead (offsets in the host line in conversation) before Finish if the task still needs unread lines.";

/// Field-level hint for `*SearchReadStep` (`ArchiveSearchReadInput`).
pub(crate) const SEARCH_READ_STEP_INPUT_DESCRIPTION: &str = "Required: archive_ref and non-empty grep. If the visible @N in conversation is only a window, emit SearchRead with the same @N, grep, and the offset= value from the host line. Do not use SearchRead for contiguous unfiltered text — use PageRead.";

/// Field-level hint for `*PageReadStep` (`ArchivePageReadInput`).
pub(crate) const PAGE_READ_STEP_INPUT_DESCRIPTION: &str = "Required: archive_ref; omit grep. Paging over full rendered lines. Use for contiguous unfiltered text or the next unfiltered window; the host line gives offset= for the next step. For locating sparse lines, use SearchRead with grep first, not long PageRead scans.";

/// Full shared prelude (FSM header, planning types, StructuredReply, archive read inputs).
pub fn render_generated_tools_prelude() -> String {
    let esc = escape_baml_description;
    let c_plan = esc(CITATIONS_PLAN_OPTIONAL);
    let c_reply = esc(CITATIONS_DECISION_OR_SYNTHESIS);
    let c_ar = esc(ARCHIVE_READ_ARCHIVE_REF);
    let c_lim = esc(ARCHIVE_READ_LIMIT);
    let c_sg = esc(ARCHIVE_SEARCH_READ_GREP);
    let c_soff = esc(ARCHIVE_SEARCH_READ_OFFSET);
    let c_poff = esc(ARCHIVE_PAGE_READ_OFFSET);
    format!(
        r#"// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. SearchRead: line-filtered archive read (grep required). Page with offset/limit over matches.
// 4. PageRead: contiguous archive paging (no grep). Page with offset/limit over full rendered lines.
// 5. Finish: Closes the session gracefully
// 6. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - SearchRead and PageRead require a real @N from a prior Send for this tool (per step field @description; see conversation_history when a windowed archive lists offset=).
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

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {{
  contract_version string
  session_open bool
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

/// Line-filtered archive read (emit op SearchRead).
class ArchiveSearchReadInput {{
  archive_ref string @description("{c_ar}")
  grep string @description("{c_sg}")
  offset int? @description("{c_soff}")
  limit int? @description("{c_lim}")
}}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {{
  archive_ref string @description("{c_ar}")
  offset int? @description("{c_poff}")
  limit int? @description("{c_lim}")
}}
"#,
        c_plan = c_plan,
        c_reply = c_reply,
        c_ar = c_ar,
        c_lim = c_lim,
        c_sg = c_sg,
        c_soff = c_soff,
        c_poff = c_poff,
    )
}
