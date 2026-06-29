// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Compaction summarizer trait and test doubles.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::context::RuntimeScope;

use super::{
    compactor::ContextCompactionService,
    types::{CompactionPrefixInput, CompactionRequest},
};

/// BAML function name in `baml_src/host/context_compaction.baml`.
pub const HOST_COMPACTION_BAML_FUNCTION: &str = "SummarizeConversationPrefix";

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

    /// Returns final wire summary (`[conversation summary]\n…\nPreserved refs: …`).
    async fn summarize_prefix(
        &self,
        scope: &RuntimeScope,
        input: &CompactionPrefixInput,
    ) -> Result<String, CompactionSummarizeError>;
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

    async fn summarize_prefix(
        &self,
        _scope: &RuntimeScope,
        input: &CompactionPrefixInput,
    ) -> Result<String, CompactionSummarizeError> {
        ContextCompactionService::finalize_summary(&self.prose, &input.source_rendered)
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
