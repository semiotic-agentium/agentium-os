// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! LLM integration for host compaction BAML (gated on `OPENROUTER_API_KEY`).

use std::{path::PathBuf, sync::Arc};

use baml_rt_a2a::{HostCompactionConfig, HostCompactionEngine, HostCompactionSource};
use baml_rt_core::bus::BusWithEffects;
use baml_rt_llm_config::{LlmClientConfig, StaticResolver, test_model_default};
use baml_rt_provenance::{
    CompactionPrefixInput, CompactionRequest, compaction_runtime_scope,
    invoke_summarizer_with_retry,
};
use baml_rt_quickjs::llm_client_registry::LlmSecretResolver;
use baml_rt_tools::{
    archive_refs::{ArchiveEntry, HistoryEntry, RefTable},
    citations::unresolved_wire_citations,
};
use test_support::testing::provenance_fixtures::{provenance_agent_id, provenance_context_id};

const HOST_CLIENTS_BAML: &str = include_str!("../../../baml_src/host/clients.baml");
const HOST_COMPACTION_BAML: &str = include_str!("../../../baml_src/host/context_compaction.baml");

const EMBEDDED_HOST_BAML: &[(&str, &str)] = &[
    ("baml_src/host/clients.baml", HOST_CLIENTS_BAML),
    (
        "baml_src/host/context_compaction.baml",
        HOST_COMPACTION_BAML,
    ),
];

struct EnvSecretResolver;

impl LlmSecretResolver for EnvSecretResolver {
    fn resolve_llm_api_key(&self, _client: &str, key: &str) -> Option<(String, String)> {
        std::env::var(key).ok().map(|v| (v, "env".into()))
    }
}

fn host_schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn boot_host_compaction_engine(source: HostCompactionSource) -> HostCompactionEngine {
    let effect_emitter: Arc<dyn baml_rt_core::bus::EffectEmitter> = Arc::new(BusWithEffects::new());
    let llm_config = LlmClientConfig::sensible_default();
    let llm_resolver = Arc::new(StaticResolver::new(
        Arc::new(llm_config),
        Arc::new(baml_rt_llm_config::FnoxFileSecretResolver::default_path_resolver()),
    ));
    HostCompactionEngine::boot(
        HostCompactionConfig { source },
        effect_emitter,
        llm_resolver,
        Arc::new(EnvSecretResolver),
    )
    .await
    .expect("boot host compaction engine")
}

fn compaction_ref_table_with_at3_and_hash2() -> Arc<RefTable> {
    let table = Arc::new(RefTable::new());
    table.insert_virtual_archive(
        3,
        ArchiveEntry::new(
            baml_rt_tools::archive_read::render_to_lines(&serde_json::json!({"blob": true})),
            "tool/archive".into(),
            None,
            "evt-archive-3".into(),
            "tool_result".into(),
        ),
    );
    table.insert_virtual_history(
        2,
        HistoryEntry::new("evt-history-2".into(), "message".into()),
        "cited history",
    );
    table
}

#[tokio::test]
async fn host_compaction_boots_from_dir_override() {
    let _engine = boot_host_compaction_engine(HostCompactionSource::Dir(host_schema_dir())).await;
}

#[tokio::test]
async fn host_compaction_baml_validates_wire_refs_after_finalize() {
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!("SKIP host_compaction_baml: OPENROUTER_API_KEY unset");
        return;
    }
    unsafe {
        std::env::set_var("BAML_TEST_MODEL", test_model_default());
    }

    let engine = Arc::new(
        boot_host_compaction_engine(HostCompactionSource::Embedded {
            root: "baml_src",
            files: EMBEDDED_HOST_BAML,
        })
        .await,
    );

    let request = CompactionRequest {
        context_id: provenance_context_id(42),
        agent_id: provenance_agent_id(),
    };
    let scope = compaction_runtime_scope(&request);
    let source = "user: inspect archive @3 and cite #2\nassistant: opened @3 for review";
    let ref_table = compaction_ref_table_with_at3_and_hash2();
    let input = CompactionPrefixInput {
        source_rendered: source.to_string(),
        active_planning_digest: None,
        recent_tail_preview: Some("user: latest ping".into()),
        ref_table: Arc::clone(&ref_table),
    };
    let summary = invoke_summarizer_with_retry(engine.as_ref(), &scope, &input)
        .await
        .expect("summarize");
    assert!(
        summary.starts_with("[conversation summary]\n"),
        "summary uses compaction envelope: {summary}"
    );
    assert!(
        unresolved_wire_citations(&summary, ref_table.as_ref()).is_empty(),
        "LLM summary must not cite unresolved wire refs: {summary}"
    );
}
