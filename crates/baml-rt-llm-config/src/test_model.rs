// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Test-model selection knob.
//!
//! The default model the workspace's `llm-tests` lane runs against is
//! controlled by the `BAML_TEST_MODEL` environment variable. When unset, the
//! built-in fallback applies, preserving historical behaviour for anyone
//! running tests without an explicit override.
//!
//! This single helper is the only place the literal lives in Rust source. The
//! same `env.BAML_TEST_MODEL` name appears in BAML `client` blocks across the
//! workspace; CI wires the variable into the test job. The pair is intended to
//! make CI resilient to provider-side policy shifts (e.g. ZDR enforcement on a
//! company OpenRouter account dropping a previously-routable model). Swapping
//! the model in CI is a single repo-variable change with no code edits.
//!
//! Reading this here, you probably want one of:
//!
//! - `BAML_TEST_MODEL=x-ai/grok-4.3 cargo nextest run …` to swap the
//!   model for a local run.
//! - Issue #429 in the tracker for the full design rationale.

/// Default test model. Anything called from a `default_*` path or a
/// `sensible_default()` constructor should route through here rather than
/// embedding a literal.
///
/// The fallback is `x-ai/grok-4.3`: it is the model the workspace's
/// agent prompts and E2E test assertions are tuned against. Forks running
/// under an OpenRouter account whose policy excludes grok should set
/// `BAML_TEST_MODEL` to a model their account can route — see #429 for the
/// design rationale and #428 for the operational tracker.
pub const FALLBACK_TEST_MODEL: &str = "x-ai/grok-4.3";

/// The model the workspace's `llm-tests` lane should target.
///
/// Reads `BAML_TEST_MODEL`. When the variable is unset or empty, returns
/// [`FALLBACK_TEST_MODEL`].
pub fn test_model_default() -> String {
    match std::env::var("BAML_TEST_MODEL") {
        Ok(s) if !s.is_empty() => s,
        _ => FALLBACK_TEST_MODEL.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_pins_the_model_tests_are_tuned_against() {
        // Locks the fallback to the model the workspace's agent prompts and
        // E2E test assertions are tuned against. Changing this is a policy
        // decision — read issue #429 before flipping it.
        assert_eq!(FALLBACK_TEST_MODEL, "x-ai/grok-4.3");
    }
}
