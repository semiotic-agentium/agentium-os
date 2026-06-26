// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Full A2A stack: stream terminal emits settlement and triggers post-turn compaction.

use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{
    A2aWireRequest,
    bus::{BusWithEffects, EffectEmitter, EffectEvent, EffectSubscriber, EffectSubscriberTier},
    collect_a2a_stream_one_shot,
    ids::{ContextId, ExternalId, MessageId},
};
use baml_rt_llm_config::{LlmClientConfig, ModelBudgetOverride};
use baml_rt_provenance::{ProvenanceContextReader, ProvenanceWriter};
use test_support::{
    common::{
        build_fixture_package_to_temp, ensure_fixture_runtime_types, send_stream_request,
        test_surreal_store,
    },
    testing::provenance_fixtures::{provenance_agent_id, provenance_context_id, wall_clock_tick},
};

fn tuned_llm_config() -> LlmClientConfig {
    let mut config = LlmClientConfig::sensible_default();
    config.compaction.defaults.item_threshold = 8;
    config.compaction.defaults.recent_tail_retention = 2;
    config.compaction.client_overrides.insert(
        "OpenRouter".to_string(),
        ModelBudgetOverride {
            context_window_tokens: Some(8192),
            trigger_ratio: Some(0.35),
            emergency_ratio: Some(0.55),
            output_reserve_tokens: Some(512),
        },
    );
    config
}

struct SettlementSpy(Arc<AtomicBool>);

#[async_trait]
impl EffectSubscriber for SettlementSpy {
    fn name(&self) -> &'static str {
        "settlement_spy"
    }

    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Background
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        if matches!(event, EffectEvent::ContextHistorySettled { .. }) {
            self.0.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

async fn seed_messages(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    ctx: &ContextId,
    agent_id: &baml_rt_core::ids::AgentId,
) {
    for i in 0..10 {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                ctx.clone(),
                MessageId::from_external(ExternalId::new(format!("stream-seed-{i}"))),
                "user".into(),
                vec![format!("seed {i} {}", "PAD ".repeat(400))],
                None,
                agent_id.clone(),
                1_910_000_000_000 + i as u64,
            ))
            .await
            .expect("seed");
        wall_clock_tick().await;
    }
}

async fn setup_stream_agent(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    bus: Arc<BusWithEffects>,
) -> A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("stream-js-tool").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-js-tool dist/index.js");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_surreal_store(store)
        .with_agent_id(provenance_agent_id())
        .with_effect_emitter(bus)
        .with_llm_client_config(Arc::new(tuned_llm_config()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(Arc::new(
            baml_rt_provenance::FixedCompactionSummarizer::new(
                "Compacted stream-settlement transcript prefix.",
            ),
        ))
        .build()
        .await
        .expect("build stream agent")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_settlement_triggers_post_turn_compaction() {
    let store = test_surreal_store().await;
    let ctx = provenance_context_id(1_910_010);
    let bus = Arc::new(BusWithEffects::new());
    let settlement_seen = Arc::new(AtomicBool::new(false));
    bus.subscribe_effect_subscriber(Arc::new(SettlementSpy(Arc::clone(&settlement_seen))))
        .await;

    let agent = setup_stream_agent(Arc::clone(&store), Arc::clone(&bus)).await;
    seed_messages(&store, &ctx, agent.agent_id()).await;
    let seeded = store
        .conversation_context(&ctx, None)
        .await
        .expect("seeded items");
    assert!(
        seeded.len() >= 8,
        "expected seeded transcript rows before stream, got {}",
        seeded.len()
    );

    let request = send_stream_request(
        "stream-settle-1",
        "stream-task: compaction-settle",
        "corr-1910010-1",
        Some(ctx.clone()),
    );
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("open stream");
    let chunks = collect_a2a_stream_one_shot(stream).await;
    assert!(
        !chunks.is_empty(),
        "stream must yield at least one chunk; got 0"
    );
    for chunk in &chunks {
        assert!(
            chunk.as_ref().get("error").is_none(),
            "stream returned error: {}",
            chunk.as_ref()
        );
    }

    let settle_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !settlement_seen.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < settle_deadline,
            "stream terminal must emit ContextHistorySettled"
        );
        tokio::task::yield_now().await;
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head")
            .is_some()
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "compaction head not written after stream terminal settlement"
        );
        tokio::task::yield_now().await;
    }
}
