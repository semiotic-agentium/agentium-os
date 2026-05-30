// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Session-side classification helpers.
//!
//! [`ClassifiedToolError`] lives in [`baml_rt_core`] and is built from **typed** integration errors
//! (`From<NotionError>` etc.) at tool boundaries. [`BamlRtError::ToolClassified`] carries that
//! surface through the runtime without string reparsing.

use std::sync::Arc;

pub use baml_rt_core::ClassifiedToolError;
use baml_rt_core::{
    BamlRtError, baml_error_disposition, retryability_for_a2a,
    semantics::{ErrorDisposition, Retryability},
};

/// Arc-wrapped callback for custom [`ClassifiedToolError`] mapping in a tool session.
pub type ToolExecutionClassifier = Arc<dyn Fn(&BamlRtError) -> ClassifiedToolError + Send + Sync>;

/// True when the host may retry this failure without a new LLM turn (bounded policy).
#[must_use]
pub fn should_host_retry(classified: &ClassifiedToolError) -> bool {
    matches!(classified.disposition, ErrorDisposition::HostRetriable)
}

/// Classify a [`BamlRtError`] for one-shot host retry (e.g. rate limits) without a new LLM turn.
#[must_use]
pub fn should_host_retry_baml_error(err: &BamlRtError) -> bool {
    matches!(baml_error_disposition(err), ErrorDisposition::HostRetriable)
}

/// Classify using an optional per-session classifier, else [`ClassifiedToolError::from_baml_error`].
///
/// [`BamlRtError::ToolClassified`] is always returned verbatim (no classifier override).
#[must_use]
pub fn classify_for_session(
    classifier: &Option<ToolExecutionClassifier>,
    err: &BamlRtError,
) -> ClassifiedToolError {
    if let BamlRtError::ToolClassified(c) = err {
        return c.clone();
    }
    classifier
        .as_ref()
        .map(|c| c(err))
        .unwrap_or_else(|| ClassifiedToolError::from_baml_error(err))
}

/// Map a [`BamlRtError`] to A2A retryability (single policy entry point).
#[must_use]
pub fn a2a_retryability(err: &BamlRtError) -> Retryability {
    retryability_for_a2a(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_execution_core_default_is_inform_continue() {
        let err = BamlRtError::ToolExecution("slack rate_limited, retry later".to_string());
        let c = ClassifiedToolError::from_baml_error(&err);
        assert_eq!(c.disposition, ErrorDisposition::InformAndContinue);
        assert!(!should_host_retry(&c));
    }

    #[test]
    fn invalid_argument_llm_correctable() {
        let err = BamlRtError::InvalidArgument("bad field".to_string());
        let c = ClassifiedToolError::from_baml_error(&err);
        assert_eq!(c.disposition, ErrorDisposition::LlmCorrectable);
        assert_eq!(c.code, "invalid_argument");
    }

    #[test]
    fn tool_classified_skips_session_classifier() {
        let inner = ClassifiedToolError {
            code: "x".into(),
            disposition: ErrorDisposition::HostRetriable,
            message: "m".into(),
            hint: None,
            retry_after_ms: None,
        };
        let err = BamlRtError::ToolClassified(inner.clone());
        let classifier: Option<ToolExecutionClassifier> = Some(Arc::new(|_| ClassifiedToolError {
            code: "wrong".into(),
            disposition: ErrorDisposition::Fatal,
            message: "ignored".into(),
            hint: None,
            retry_after_ms: None,
        }));
        let got = classify_for_session(&classifier, &err);
        assert_eq!(got, inner);
    }
}
