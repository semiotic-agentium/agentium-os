//! Archive / read-before-repeat policy prepended to **generated** step-executor phase prompts.
//!
//! The policy text is embedded **literally** at the start of each per-phase `prompt` body (see
//! [`SESSION_STEP_STABLE_PREFIX_BAML`]).
//!
//! Step-executor hops that call `invoke_function_with_intra` on `BamlRuntimeManager` also receive
//! optional `ctx.tags['tool_schema_prelude']` with the merged
//! `baml_src/_baml_runtime.baml` text so phase prompts can place authoritative tool/step **types**
//! before history without duplicating `{{ ctx.output_format }}` JSON at the tail. Plain
//! `invoke_function` paths still use `ctx.tags['conversation_transcript']` only unless extended
//! elsewhere.

/// Place at the **start** of generated step-executor `prompt` bodies (before the phase cue and
/// parent IR template). Same prose historically injected under `session_step_stable_prefix`; now
/// inlined so `ctx.tags` carries history only via `conversation_transcript`.
pub const SESSION_STEP_STABLE_PREFIX_BAML: &str = "Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.\n\n\
     For `op: \"Open\"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.\n\n";

/// Alias for callers that referred to the injected-string constant by value name.
pub const SESSION_STEP_STABLE_PREFIX_VALUE: &str = SESSION_STEP_STABLE_PREFIX_BAML;

/// `ctx.tags` key for merged-runtime BAML text (tool cards + step classes) on step-executor hops.
pub const TOOL_SCHEMA_PRELUDE_TAG: &str = "tool_schema_prelude";

/// `ctx.tags` key for the projected conversation transcript on intra-turn / step-executor hops.
pub const CONVERSATION_TRANSCRIPT_TAG: &str = "conversation_transcript";
