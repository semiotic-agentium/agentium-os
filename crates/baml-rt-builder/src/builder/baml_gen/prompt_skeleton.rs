//! Thin facade over [`prompt_compositor::PromptCompositor`] kept for callers that only need the
//! authored-rewriter entry point. New code should use [`PromptCompositor`] directly so prompt
//! composition flows through the single domain object.
//!
//! - [`canonical_prompt_prefix_jinja`] returns [`PromptCompositor::canonical_opening`] — the
//!   byte-identical prefix every model-facing prompt opens with.
//! - [`rewrite_authored_prompt_body`] delegates to [`PromptCompositor::authored_non_fsm`].

use super::prompt_compositor::PromptCompositor;

/// Stable archive prefix + catalog if-block. Same bytes for every prompt — generated phase
/// executors and rewritten author prompts share this opening so OpenAI prefix-cache aligns.
pub fn canonical_prompt_prefix_jinja() -> String {
    PromptCompositor::canonical_opening()
}

/// Wrap a hand-authored prompt body in the canonical structured skeleton.
pub fn rewrite_authored_prompt_body(author_inner: &str) -> String {
    PromptCompositor::authored_non_fsm(author_inner)
}
