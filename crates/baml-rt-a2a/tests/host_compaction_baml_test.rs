// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! LLM integration for host compaction BAML (gated on `OPENROUTER_API_KEY`).

use std::{path::PathBuf, sync::Arc};

use baml_rt_a2a::{HostCompactionConfig, HostCompactionEngine};
use baml_rt_core::bus::BusWithEffects;
use baml_rt_llm_config::{LlmClientConfig, StaticResolver, test_model_default};
use baml_rt_provenance::{
    CompactionPrefixInput, CompactionRequest, ContextCompactionService,
    ConversationCompactionSummarizer, compaction_runtime_scope,
};
use baml_rt_quickjs::llm_client_registry::LlmSecretResolver;
use test_support::testing::provenance_fixtures::{provenance_agent_id, provenance_context_id};

struct EnvSecretResolver;

impl LlmSecretResolver for EnvSecretResolver {
    fn resolve_llm_api_key(&self, _client: &str, key: &str) -> Option<(String, String)> {
        std::env::var(key).ok().map(|v| (v, "env".into()))
    }
}

fn host_schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[tokio::test]
async fn host_compaction_baml_preserves_wire_refs_after_finalize() {
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!("SKIP host_compaction_baml: OPENROUTER_API_KEY unset");
        return;
    }
    unsafe {
        std::env::set_var("BAML_TEST_MODEL", test_model_default());
    }

    let effect_emitter: Arc<dyn baml_rt_core::bus::EffectEmitter> = Arc::new(BusWithEffects::new());
    let llm_config = LlmClientConfig::sensible_default();
    let llm_resolver = Arc::new(StaticResolver::new(
        Arc::new(llm_config),
        Arc::new(baml_rt_llm_config::FnoxFileSecretResolver::default_path_resolver()),
    ));
    let engine = Arc::new(
        HostCompactionEngine::boot(
            HostCompactionConfig {
                schema_dir: host_schema_dir(),
            },
            effect_emitter,
            llm_resolver,
            Arc::new(EnvSecretResolver),
        )
        .await
        .expect("boot host compaction engine"),
    );

    let request = CompactionRequest {
        context_id: provenance_context_id(42),
        agent_id: provenance_agent_id(),
    };
    let scope = compaction_runtime_scope(&request);
    let source = "user: inspect archive @3 and cite #2\nassistant: opened @3 for review";
    let input = CompactionPrefixInput {
        source_rendered: source.to_string(),
        active_planning_digest: None,
        recent_tail_preview: Some("user: latest ping".into()),
    };
    let summary = engine
        .summarize_prefix(&scope, &input)
        .await
        .expect("summarize");
    assert!(summary.contains("@3"));
    assert!(summary.contains("#2"));
    ContextCompactionService::finalize_summary("sanity", source).expect("finalize contract");
}
