//! Single source for repeated BAML `@description` text (citations, SearchRead/PageRead archive policy).
//! [`render_generated_tools_prelude`] emits the shared prelude; [`tool_interfaces`] uses the same
//! citation/send/read strings for per-tool session classes.
//!
//! ## Prompt style (generated `@description` and FSM comments)
//!
//! - **Voice:** Imperative, second person implied. No hedging for host-enforced behaviour.
//! - **Strength:** **Must** / **Do not** — schema and namespace rules. **Use** / **Set** — operational
//!   defaults. Read-tactics for windowed tool output are stated on the FSM step fields and in the
//!   in-context `offset=` / pagination line in the projected transcript, not in the static prelude. **Optional** — fields
//!   the host allows empty (`[]`, omit).
//! - **Verbs:** **Emit** — one FSM step or session-plan fragment. **Return** — reserved for human
//!   doc / coordination intro (“return a report”) where it matches BAML wording; step rules use *emit*.
//! - **Refs:** Preserve `#N` vs `@N` and `!` exactly — see `docs/how-to-write-agents.md` §6.
//! - **Per-hop FSM:** Hand-written and generated prompts must not restate Open/Send/Read *field* JSON
//!   or a second FSM that could disagree with the BAML return type. Legal ops and payloads: narrowed
//!   return union and `*SendInput` / `*OpenInput` / archive classes in the merged runtime prelude only
//!   (see `docs/how-to-write-agents.md` — prompt layout for session step functions).
//! - **Step-executor archive policy:** [`baml_rt_tools::session_ctx_tags::SESSION_STEP_STABLE_PREFIX_BAML`]
//!   is prepended literally to generated per-phase `prompt` bodies in `session_from_ir`. History
//!   uses **`ctx.tags['conversation_transcript']` only** — no other `ctx.tags` keys are injected for
//!   step-executor hops.

use super::escape::escape_baml_description;

// --- Citations (#N / @N / line suffixes) ---

/// `StructuredReply.citations` and aligned session-plan decision citations (same ref rules).
pub(crate) const CITATIONS_DECISION_OR_SYNTHESIS: &str = "Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.";

/// `StandardStructuredPlan.citations` (optional on plans).
pub(crate) const CITATIONS_PLAN_OPTIONAL: &str = "Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].";

/// `*SendStep.citations`
pub(crate) const CITATIONS_SEND_STEP: &str = "Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.";

// --- Archive access (SearchRead = filter, PageRead = contiguous paging) ---

pub(crate) const ARCHIVE_READ_ARCHIVE_REF: &str = "archive_ref names a Send output archive: set it to the exact @N shown on that Send line in the transcript (e.g. @1, @2). Do not use #N or any # prefix — # is for session history lines only and is invalid in this field. Do not invent @N. PageRead/SearchRead materialize line bodies from that handle; allowed with a real visible @N without Open or an active session.";

pub(crate) const ARCHIVE_SEARCH_READ_GREP: &str = "Non-empty filter over rendered archive lines. SearchRead is for locating lines that match; use when you have a concrete term. Shape: substring or regex per host (e.g. name, -i error). For full contiguous slices without filtering, use PageRead.";

pub(crate) const ARCHIVE_SEARCH_READ_OFFSET: &str = "0-based offset over grep matches (after the filter). First window: offset=0; next: previous next_offset while matches remain.";

pub(crate) const ARCHIVE_PAGE_READ_OFFSET: &str = "0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.";

pub(crate) const ARCHIVE_READ_LIMIT: &str = "Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).";

// --- Conversation history (ctx.tags) — must match runtime projection ---

/// Canonical Jinja for past turns: inject [`format_conversation_history_transcript`]
/// (`baml_rt_tools::prompt_projection`) as `ctx.tags['conversation_transcript']` — `role: content`
/// per turn, blank line between turns. This is the **only** history tag BAML receives.
///
/// **Session-plan parents** (`Choose*` → `*SessionPlan`): do **not** paste transcript `{% if %}` blocks
/// or Open/Send/SearchRead field-shape litanies — generated `__entry` / `__active__*`
/// inject archive policy, optional `tool_schema_prelude`, narrowed-type footer, transcript, and phase
/// constraints. Author only task- and domain-specific lines (IDs, safety rules, channel/thread form).
///
/// For **non-phase** BAML (classifiers, planners, synthesis), keep order: task lines →
/// `{{ ctx.tags['conversation_transcript'] }}` when needed → `{{ ctx.output_format }}` last.
#[expect(dead_code, reason = "authoring reference for non-phase prompts")]
pub(crate) const BAML_CONVERSATION_HISTORY_JINJA_BLOCK: &str = r#"{{ ctx.tags['conversation_transcript'] }}
"#;

// --- Session plan `step` field guidance ---

pub(crate) const STEP_DESC_CLAUDE_OR_A2A: &str = "Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes above — do not paraphrase them in prose. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. For A2A Send, `input.text` is required when sending.";

pub(crate) const STEP_DESC_DEFAULT: &str = "Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only.";

/// Field-level hint for `*SearchReadStep` (`ArchiveSearchReadInput`).
pub(crate) const SEARCH_READ_STEP_INPUT_DESCRIPTION: &str = "SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.";

/// Field-level hint for `*PageReadStep` (`ArchivePageReadInput`).
pub(crate) const PAGE_READ_STEP_INPUT_DESCRIPTION: &str = "PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.";

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
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan parent `prompt`: task + domain lines only; generated per-phase executors
// add archive prefix, optional tool_schema_prelude, narrowed union, transcript, and output binding.

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
  status string @description("FSM status: awaiting_open | just_opened | done")
  last_step_op string? @description("Previous FSM step op when known: open | send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
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

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {{
  op "SearchRead"
  input ArchiveSearchReadInput @description("{search_desc}")
}}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {{
  op "PageRead"
  input ArchivePageReadInput @description("{page_desc}")
}}

/// Terminal hop without a host tool session: answer from visible conversation archives only.
/// Do not emit when you need a fresh Open/Send — use ENTRY Open or ACTIVE Send instead.
class ReadOnlyFinishStep {{
  op "ReadOnlyFinish"
  reply StructuredReply @description("User-visible answer; citations must include every @N archive line evidence relied on — omitting citations when grounding on archives is invalid.")
}}
"#,
        c_plan = c_plan,
        c_reply = c_reply,
        c_ar = c_ar,
        c_lim = c_lim,
        c_sg = c_sg,
        c_soff = c_soff,
        c_poff = c_poff,
        search_desc = esc(SEARCH_READ_STEP_INPUT_DESCRIPTION),
        page_desc = esc(PAGE_READ_STEP_INPUT_DESCRIPTION),
    )
}
