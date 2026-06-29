// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Boot shared host BAML compaction summarizer for the runner.

use std::{path::PathBuf, sync::Arc};

use baml_rt_a2a::{HostCompactionConfig, HostCompactionEngine};
use baml_rt_core::{Result, bus::EffectEmitter};
use baml_rt_llm_config::{StaticResolver, load_stored_config};
use baml_rt_provenance::ConversationCompactionSummarizer;
use baml_rt_quickjs::llm_resolver_adapter::SecretResolverToLlmAdapter;

use crate::config::ProvenanceConfig;

fn default_host_schema_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BAML_HOST_SCHEMA_DIR") {
        return PathBuf::from(dir);
    }
    const CONTAINER_DEFAULT: &str = "/opt/agentium";
    let container_root = PathBuf::from(CONTAINER_DEFAULT);
    if container_root.join("baml_src").is_dir() {
        return container_root;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) async fn boot_compaction_summarizer(
    provenance_config: &ProvenanceConfig,
    effect_emitter: Arc<dyn EffectEmitter>,
) -> Result<Arc<dyn ConversationCompactionSummarizer>> {
    let llm_config = load_stored_config(provenance_config.config_service().as_ref()).await;
    tracing::info!(
        default = %llm_config.default,
        clients = llm_config.clients.len(),
        agent_overrides = llm_config.overrides.agent.len(),
        function_overrides = llm_config.overrides.agent_function.len(),
        "LLM client config loaded for host compaction"
    );
    let llm_resolver = Arc::new(StaticResolver::new(
        Arc::new(llm_config),
        provenance_config.llm_secret_resolver(),
    ));
    let llm_secret = Arc::new(SecretResolverToLlmAdapter::new(
        provenance_config.llm_secret_resolver(),
    ));
    let engine = HostCompactionEngine::boot(
        HostCompactionConfig {
            schema_dir: default_host_schema_dir(),
        },
        effect_emitter,
        llm_resolver,
        llm_secret,
    )
    .await?;
    Ok(Arc::new(engine))
}
