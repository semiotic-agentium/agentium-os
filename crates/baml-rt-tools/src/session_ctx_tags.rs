//! Archive / read-before-repeat policy prepended to **generated** step-executor phase prompts.
//!
//! The policy text is embedded **literally** at the start of each per-phase `prompt` body (see
//! [`SESSION_STEP_STABLE_PREFIX_BAML`]). It is **not** passed via `ctx.tags` — the only tag the
//! host uses for conversational history is [`crate::prompt_projection`] →
//! `ctx.tags['conversation_transcript']`.

/// Place at the **start** of generated step-executor `prompt` bodies (before the phase cue and
/// parent IR template). Same prose historically injected under `session_step_stable_prefix`; now
/// inlined so `ctx.tags` carries history only via `conversation_transcript`.
pub const SESSION_STEP_STABLE_PREFIX_BAML: &str = "Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.\n\n\
     For `op: \"Open\"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.\n\n";

/// Alias for callers that referred to the injected-string constant by value name.
pub const SESSION_STEP_STABLE_PREFIX_VALUE: &str = SESSION_STEP_STABLE_PREFIX_BAML;
