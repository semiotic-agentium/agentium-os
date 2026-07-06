// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-owned BAML compaction engine (implements [`ConversationCompactionSummarizer`]).

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{Result, bus::EffectEmitter, context::RuntimeScope};
use baml_rt_llm_config::LlmClientResolver;
use baml_rt_provenance::{
    CompactionPrefixInput, CompactionSummarizeError, ConversationCompactionSummarizer,
    HOST_COMPACTION_BAML_FUNCTION, finalize_compaction_summary,
};
use baml_rt_quickjs::{BamlRuntimeManager, llm_client_registry::LlmSecretResolver};
use serde_json::{Value, json};
use tokio::sync::RwLock;

pub enum HostCompactionSource {
    /// Load host BAML from embedded, compiled-in content.
    Embedded {
        root: &'static str,
        files: &'static [(&'static str, &'static str)],
    },
    /// Load from an on-disk `<dir>/baml_src` (dev/operator override).
    Dir(PathBuf),
}

pub struct HostCompactionConfig {
    pub source: HostCompactionSource,
}

/// Shared host BAML runtime loaded with `SummarizeConversationPrefix`.
pub struct HostCompactionEngine {
    runtime: Arc<RwLock<BamlRuntimeManager>>,
}

impl HostCompactionEngine {
    pub async fn boot(
        config: HostCompactionConfig,
        effect_emitter: Arc<dyn EffectEmitter>,
        llm_resolver: Arc<dyn LlmClientResolver>,
        llm_secret_resolver: Arc<dyn LlmSecretResolver>,
    ) -> Result<Self> {
        let mut manager = BamlRuntimeManager::builder()
            .with_llm_secret_resolver(llm_secret_resolver)
            .build()?;
        manager.set_llm_client_resolver(llm_resolver);
        manager.set_effect_emitter(effect_emitter);
        match config.source {
            HostCompactionSource::Embedded { root, files } => {
                manager.load_schema_from_files(root, files)?;
            }
            HostCompactionSource::Dir(schema_dir) => {
                let schema_path = schema_dir.to_str().ok_or_else(|| {
                    baml_rt_core::BamlRtError::InvalidArgument(
                        "host compaction schema path is not valid UTF-8".into(),
                    )
                })?;
                manager.load_schema(schema_path)?;
            }
        }
        Ok(Self {
            runtime: Arc::new(RwLock::new(manager)),
        })
    }
}

/// Build JSON args for [`HOST_COMPACTION_BAML_FUNCTION`].
#[must_use]
pub fn compaction_baml_invoke_args(
    input: &CompactionPrefixInput,
    validation_feedback: Option<&str>,
) -> Value {
    json!({
        "transcript_prefix": input.source_rendered,
        "active_planning_digest": input.active_planning_digest,
        "recent_tail_preview": input.recent_tail_preview,
        "validation_feedback": validation_feedback,
    })
}

#[async_trait]
impl ConversationCompactionSummarizer for HostCompactionEngine {
    fn backend_label(&self) -> &'static str {
        "llm"
    }

    async fn summarize_prefix_attempt(
        &self,
        scope: &RuntimeScope,
        input: &CompactionPrefixInput,
        validation_feedback: Option<String>,
    ) -> std::result::Result<String, CompactionSummarizeError> {
        let args = compaction_baml_invoke_args(input, validation_feedback.as_deref());
        let guard = self.runtime.read().await;
        let value = guard
            .invoke_host_function(scope, HOST_COMPACTION_BAML_FUNCTION, args)
            .await
            .map_err(|e| CompactionSummarizeError::LlmInvoke(e.to_string()))?;
        let prose = value.as_str().map(str::to_owned).ok_or_else(|| {
            CompactionSummarizeError::InvalidOutput(format!(
                "expected string from {HOST_COMPACTION_BAML_FUNCTION}, got {value}"
            ))
        })?;
        finalize_compaction_summary(&prose, &input.ref_table)
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_baml_invoke_args_include_validation_feedback() {
        let input = CompactionPrefixInput {
            source_rendered: "user: hello".into(),
            active_planning_digest: Some("plan".into()),
            recent_tail_preview: Some("tail".into()),
            ref_table: Arc::new(baml_rt_tools::archive_refs::RefTable::new()),
        };
        let args = compaction_baml_invoke_args(&input, Some("unresolved @9"));
        assert_eq!(args["transcript_prefix"], "user: hello");
        assert_eq!(args["active_planning_digest"], "plan");
        assert_eq!(args["recent_tail_preview"], "tail");
        assert_eq!(args["validation_feedback"], "unresolved @9");

        let args = compaction_baml_invoke_args(&input, None);
        assert!(args["validation_feedback"].is_null());
    }
}
