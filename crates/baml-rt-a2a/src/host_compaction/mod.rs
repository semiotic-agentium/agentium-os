// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-owned BAML compaction engine (implements [`ConversationCompactionSummarizer`]).

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{Result, bus::EffectEmitter, context::RuntimeScope};
use baml_rt_llm_config::LlmClientResolver;
use baml_rt_provenance::{
    CompactionPrefixInput, CompactionSummarizeError, ContextCompactionService,
    ConversationCompactionSummarizer, HOST_COMPACTION_BAML_FUNCTION,
};
use baml_rt_quickjs::{BamlRuntimeManager, llm_client_registry::LlmSecretResolver};
use serde_json::json;
use tokio::sync::RwLock;

/// Directory whose `baml_src/` subtree contains host compaction BAML (repo root or package root).
pub struct HostCompactionConfig {
    pub schema_dir: std::path::PathBuf,
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
        let schema_path = config.schema_dir.to_str().ok_or_else(|| {
            baml_rt_core::BamlRtError::InvalidArgument(
                "host compaction schema path is not valid UTF-8".into(),
            )
        })?;
        manager.load_schema(schema_path)?;
        Ok(Self {
            runtime: Arc::new(RwLock::new(manager)),
        })
    }

    async fn invoke_summarize_prose(
        &self,
        scope: &RuntimeScope,
        input: &CompactionPrefixInput,
    ) -> std::result::Result<String, CompactionSummarizeError> {
        let args = json!({
            "transcript_prefix": input.source_rendered,
            "active_planning_digest": input.active_planning_digest,
            "recent_tail_preview": input.recent_tail_preview,
        });
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
        ContextCompactionService::finalize_summary(&prose, &input.source_rendered)
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
    }
}

#[async_trait]
impl ConversationCompactionSummarizer for HostCompactionEngine {
    fn backend_label(&self) -> &'static str {
        "llm"
    }

    async fn summarize_prefix(
        &self,
        scope: &RuntimeScope,
        input: &CompactionPrefixInput,
    ) -> std::result::Result<String, CompactionSummarizeError> {
        self.invoke_summarize_prose(scope, input).await
    }
}
