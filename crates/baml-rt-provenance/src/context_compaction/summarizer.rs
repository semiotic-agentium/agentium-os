// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Compaction summarizer trait and test doubles.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::context::RuntimeScope;

use super::{
    compactor::finalize_compaction_summary,
    types::{CompactionPrefixInput, CompactionRequest},
};

/// BAML function name in `baml_src/host/context_compaction.baml`.
pub const HOST_COMPACTION_BAML_FUNCTION: &str = "SummarizeConversationPrefix";

/// LLM attempts per compaction trigger when wire-ref validation fails (initial + one feedback retry).
pub const COMPACTION_VALIDATION_RETRY_LIMIT: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum CompactionSummarizeError {
    #[error("llm invoke: {0}")]
    LlmInvoke(String),
    #[error("invalid output: {0}")]
    InvalidOutput(String),
    #[error("validation: {0}")]
    Validation(String),
}

impl CompactionSummarizeError {
    #[must_use]
    pub fn metric_label(&self) -> &'static str {
        match self {
            Self::LlmInvoke(_) => "llm_invoke",
            Self::InvalidOutput(_) => "invalid_output",
            Self::Validation(_) => "validation",
        }
    }
}

#[async_trait]
pub trait ConversationCompactionSummarizer: Send + Sync {
    /// `"llm"` (production) or `"fixed"` (test mock).
    fn backend_label(&self) -> &'static str;

    /// Single summarizer attempt. `validation_feedback` is set on retry after validator rejection.
    async fn summarize_prefix_attempt(
        &self,
        scope: &RuntimeScope,
        input: &CompactionPrefixInput,
        validation_feedback: Option<String>,
    ) -> Result<String, CompactionSummarizeError>;
}

/// Run up to [`COMPACTION_VALIDATION_RETRY_LIMIT`] attempts, feeding validation errors back on retry.
pub async fn invoke_summarizer_with_retry(
    summarizer: &dyn ConversationCompactionSummarizer,
    scope: &RuntimeScope,
    input: &CompactionPrefixInput,
) -> Result<String, CompactionSummarizeError> {
    summarize_with_validation_retry(|validation_feedback| {
        summarizer.summarize_prefix_attempt(scope, input, validation_feedback)
    })
    .await
}

/// Run up to [`COMPACTION_VALIDATION_RETRY_LIMIT`] LLM attempts, feeding validation errors back on retry.
pub async fn summarize_with_validation_retry<F, Fut>(
    mut invoke: F,
) -> Result<String, CompactionSummarizeError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<String, CompactionSummarizeError>>,
{
    let mut validation_feedback: Option<String> = None;

    for attempt in 0..COMPACTION_VALIDATION_RETRY_LIMIT {
        match invoke(validation_feedback.clone()).await {
            Ok(summary) => return Ok(summary),
            Err(CompactionSummarizeError::Validation(msg)) => {
                if attempt + 1 < COMPACTION_VALIDATION_RETRY_LIMIT {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %msg,
                        "compaction summary validation failed; retrying LLM with feedback"
                    );
                    validation_feedback = Some(msg);
                    continue;
                }
                return Err(CompactionSummarizeError::Validation(msg));
            }
            Err(err) => return Err(err),
        }
    }

    Err(CompactionSummarizeError::Validation(
        validation_feedback.unwrap_or_else(|| "compaction summary validation failed".into()),
    ))
}

/// Test-only: returns configured prose through `finalize_summary`.
pub struct FixedCompactionSummarizer {
    pub prose: String,
}

impl FixedCompactionSummarizer {
    pub fn new(prose: impl Into<String>) -> Self {
        Self {
            prose: prose.into(),
        }
    }

    /// Shared test stub for agent builds under `cfg(test)`.
    #[must_use]
    pub fn test_stub() -> Arc<dyn ConversationCompactionSummarizer> {
        Arc::new(Self::new(
            "Prior conversation was compacted; continue from recent context.",
        ))
    }
}

#[async_trait]
impl ConversationCompactionSummarizer for FixedCompactionSummarizer {
    fn backend_label(&self) -> &'static str {
        "fixed"
    }

    async fn summarize_prefix_attempt(
        &self,
        _scope: &RuntimeScope,
        input: &CompactionPrefixInput,
        _validation_feedback: Option<String>,
    ) -> Result<String, CompactionSummarizeError> {
        finalize_compaction_summary(&self.prose, &input.ref_table)
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
    }
}

/// Build a stable synthetic scope for compaction LLM routing and effect attribution.
#[must_use]
pub fn compaction_runtime_scope(request: &CompactionRequest) -> RuntimeScope {
    use baml_rt_core::ids::{ExternalId, MessageId};
    RuntimeScope::message_scope(
        request.context_id.clone(),
        request.agent_id.clone(),
        MessageId::from_external(ExternalId::new(format!(
            "host-compaction:{}",
            request.context_id.as_str()
        ))),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use test_support::testing::provenance_fixtures::{provenance_agent_id, provenance_context_id};

    use super::*;

    struct FeedbackRecordingSummarizer {
        attempts: Arc<AtomicUsize>,
        feedback_seen: Arc<std::sync::Mutex<Option<String>>>,
    }

    #[async_trait]
    impl ConversationCompactionSummarizer for FeedbackRecordingSummarizer {
        fn backend_label(&self) -> &'static str {
            "feedback-recording"
        }

        async fn summarize_prefix_attempt(
            &self,
            _scope: &RuntimeScope,
            input: &CompactionPrefixInput,
            validation_feedback: Option<String>,
        ) -> Result<String, CompactionSummarizeError> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                assert!(validation_feedback.is_none());
                Err(CompactionSummarizeError::Validation(
                    "compaction summary cites unresolved wire refs: @9".into(),
                ))
            } else {
                assert_eq!(
                    validation_feedback.as_deref(),
                    Some("compaction summary cites unresolved wire refs: @9")
                );
                *self.feedback_seen.lock().unwrap() = validation_feedback;
                finalize_compaction_summary("recovered without refs", &input.ref_table)
                    .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
            }
        }
    }

    #[tokio::test]
    async fn invoke_summarizer_with_retry_feeds_feedback_then_succeeds() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let feedback_seen = Arc::new(std::sync::Mutex::new(None::<String>));
        let summarizer = FeedbackRecordingSummarizer {
            attempts: Arc::clone(&attempts),
            feedback_seen: Arc::clone(&feedback_seen),
        };
        let request = CompactionRequest {
            context_id: provenance_context_id(1_900_001),
            agent_id: provenance_agent_id(),
        };
        let scope = compaction_runtime_scope(&request);
        let input = CompactionPrefixInput {
            source_rendered: String::new(),
            active_planning_digest: None,
            recent_tail_preview: None,
            ref_table: Arc::new(baml_rt_tools::archive_refs::RefTable::new()),
        };

        let summary = invoke_summarizer_with_retry(&summarizer, &scope, &input)
            .await
            .expect("second attempt succeeds");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            feedback_seen.lock().unwrap().as_deref(),
            Some("compaction summary cites unresolved wire refs: @9")
        );
        assert!(summary.contains("recovered without refs"));
    }

    #[tokio::test]
    async fn validation_retry_returns_error_after_exhausted_attempts() {
        struct AlwaysFailValidation;

        #[async_trait]
        impl ConversationCompactionSummarizer for AlwaysFailValidation {
            fn backend_label(&self) -> &'static str {
                "always-fail"
            }

            async fn summarize_prefix_attempt(
                &self,
                _scope: &RuntimeScope,
                _input: &CompactionPrefixInput,
                _validation_feedback: Option<String>,
            ) -> Result<String, CompactionSummarizeError> {
                Err(CompactionSummarizeError::Validation(
                    "compaction summary cites unresolved wire refs: @7".into(),
                ))
            }
        }

        let request = CompactionRequest {
            context_id: provenance_context_id(1_900_002),
            agent_id: provenance_agent_id(),
        };
        let scope = compaction_runtime_scope(&request);
        let input = CompactionPrefixInput {
            source_rendered: String::new(),
            active_planning_digest: None,
            recent_tail_preview: None,
            ref_table: Arc::new(baml_rt_tools::archive_refs::RefTable::new()),
        };

        let err = invoke_summarizer_with_retry(&AlwaysFailValidation, &scope, &input)
            .await
            .expect_err("both attempts fail validation");

        assert!(matches!(err, CompactionSummarizeError::Validation(_)));
        assert!(err.to_string().contains("@7"));
    }

    #[tokio::test]
    async fn validation_retry_does_not_retry_llm_invoke_errors() {
        struct InvokeOnceFails {
            attempts: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ConversationCompactionSummarizer for InvokeOnceFails {
            fn backend_label(&self) -> &'static str {
                "invoke-once"
            }

            async fn summarize_prefix_attempt(
                &self,
                _scope: &RuntimeScope,
                _input: &CompactionPrefixInput,
                _validation_feedback: Option<String>,
            ) -> Result<String, CompactionSummarizeError> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Err(CompactionSummarizeError::LlmInvoke("network".into()))
            }
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let summarizer = InvokeOnceFails {
            attempts: Arc::clone(&attempts),
        };
        let request = CompactionRequest {
            context_id: provenance_context_id(1_900_003),
            agent_id: provenance_agent_id(),
        };
        let scope = compaction_runtime_scope(&request);
        let input = CompactionPrefixInput {
            source_rendered: String::new(),
            active_planning_digest: None,
            recent_tail_preview: None,
            ref_table: Arc::new(baml_rt_tools::archive_refs::RefTable::new()),
        };

        let err = invoke_summarizer_with_retry(&summarizer, &scope, &input)
            .await
            .expect_err("invoke errors are not retried");

        assert!(matches!(err, CompactionSummarizeError::LlmInvoke(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
