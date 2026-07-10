// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Boot shared host BAML compaction summarizer for the runner.

use std::{path::PathBuf, sync::Arc};

use baml_rt_a2a::{HostCompactionConfig, HostCompactionEngine, HostCompactionSource};
use baml_rt_core::{Result, bus::EffectEmitter};
use baml_rt_llm_config::{StaticResolver, load_stored_config};
use baml_rt_provenance::ConversationCompactionSummarizer;
use baml_rt_quickjs::llm_resolver_adapter::SecretResolverToLlmAdapter;

use crate::config::ProvenanceConfig;

const HOST_CLIENTS_BAML: &str = include_str!("../../../baml_src/host/clients.baml");
const HOST_COMPACTION_BAML: &str = include_str!("../../../baml_src/host/context_compaction.baml");

const EMBEDDED_HOST_BAML: &[(&str, &str)] = &[
    ("baml_src/host/clients.baml", HOST_CLIENTS_BAML),
    (
        "baml_src/host/context_compaction.baml",
        HOST_COMPACTION_BAML,
    ),
];

fn host_compaction_source_from(env_override: Option<String>) -> HostCompactionSource {
    match env_override {
        Some(dir) => HostCompactionSource::Dir(PathBuf::from(dir)),
        None => HostCompactionSource::Embedded {
            root: "baml_src",
            files: EMBEDDED_HOST_BAML,
        },
    }
}

fn host_compaction_source() -> HostCompactionSource {
    host_compaction_source_from(std::env::var("BAML_HOST_SCHEMA_DIR").ok())
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
            source: host_compaction_source(),
        },
        effect_emitter,
        llm_resolver,
        llm_secret,
    )
    .await?;
    Ok(Arc::new(engine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_compaction_source_uses_dir_override() {
        match host_compaction_source_from(Some("/tmp/host-baml".into())) {
            HostCompactionSource::Dir(path) => assert_eq!(path, PathBuf::from("/tmp/host-baml")),
            HostCompactionSource::Embedded { .. } => panic!("expected dir source"),
        }
    }

    #[test]
    fn host_compaction_source_defaults_to_embedded() {
        match host_compaction_source_from(None) {
            HostCompactionSource::Embedded { root, files } => {
                assert_eq!(root, "baml_src");
                assert_eq!(files.len(), 2);
                assert!(
                    files
                        .iter()
                        .any(|(path, _)| *path == "baml_src/host/clients.baml")
                );
                assert!(
                    files
                        .iter()
                        .any(|(path, _)| *path == "baml_src/host/context_compaction.baml")
                );
            }
            HostCompactionSource::Dir(_) => panic!("expected embedded source"),
        }
    }
}
