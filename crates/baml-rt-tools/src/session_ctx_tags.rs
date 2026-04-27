//! `ctx.tags` contract for per-phase step-executor BAML (`__select` / `__act__` / `__continue__*`).
//!
//! The QuickJS host injects [`SESSION_STEP_STABLE_PREFIX_VALUE`] under
//! [`CTX_TAG_SESSION_STEP_STABLE_PREFIX`] on every `invoke_function_with_intra` call (the step-executor
//! FSM path). Codegen places [`SESSION_STEP_STABLE_PREFIX_BAML`] at the start of each generated phase
//! `prompt` so the policy is not embedded twice and phase hops share an identical policy prefix.

/// `ctx.tags` key for the shared archive / read-before-repeat policy string.
pub const CTX_TAG_SESSION_STEP_STABLE_PREFIX: &str = "session_step_stable_prefix";

/// Place at the **start** of generated step-executor `prompt` bodies (before the IR `prompt_template`,
/// and before `conversation_transcript` / `output_format` in hand-authored templates).
pub const SESSION_STEP_STABLE_PREFIX_BAML: &str =
    "{{ ctx.tags['session_step_stable_prefix'] }}\n\n";

/// Injected on the step-executor BAML path only. Must match the historical embedded preamble text
/// (now host-owned so it can be updated in one place).
pub const SESSION_STEP_STABLE_PREFIX_VALUE: &str = "Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.\n\n";
