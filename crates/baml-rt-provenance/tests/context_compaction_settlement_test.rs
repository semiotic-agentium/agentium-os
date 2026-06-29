// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! I1/I2/I3: compaction at history settlement with event-driven and planning transcripts.

use std::sync::Arc;

use baml_rt_conversation::{
    planning::{PlanningEventContent, PlanningEventKind},
    view::{ConversationItemContent, ProvenanceConversationContextItem},
};
use baml_rt_core::{
    bus::{BusWithEffects, EffectEmitter, EffectEvent},
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId},
};
use baml_rt_llm_config::{CompactionTriggerPolicy, LlmClientConfig, ModelBudgetOverride};
use baml_rt_provenance::{
    ContextCompactionSubscriber, FixedCompactionSummarizer, ProvenanceContextReader,
    ProvenanceWriter, estimate_compaction_prompt_bytes, item_is_live_planning_obligation,
    partition_items_for_compaction, render_items_for_context, render_items_with_ref_table,
    select_compactable_range,
};
use baml_rt_tools::{archive_refs::RefTable, tools::ToolRegistry};
use test_support::testing::provenance_fixtures::{
    build_isolated_store, provenance_agent_id, provenance_context_id, wall_clock_tick,
};

fn compaction_subscriber(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    _agent_id: AgentId,
) -> Arc<ContextCompactionSubscriber> {
    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    Arc::new(ContextCompactionSubscriber::new(
        store,
        writer,
        Arc::new(tuned_llm_config()),
        None,
        Arc::new(FixedCompactionSummarizer::new(
            "Compacted prior host ingress and chat history.",
        )),
        Arc::new(ToolRegistry::new()),
    ))
}

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

#[tokio::test]
async fn context_history_settled_triggers_post_turn_compaction() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_900_001);
    let agent_id = provenance_agent_id();
    let bus = BusWithEffects::new();
    let subscriber = compaction_subscriber(Arc::clone(&store), agent_id.clone());
    bus.subscribe_effect(subscriber).await;

    for i in 0..10 {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                ctx.clone(),
                MessageId::from_external(ExternalId::new(format!("settle-msg-{i}"))),
                "user".into(),
                vec![format!("status ping {i} {}", "FILL ".repeat(400))],
                None,
                agent_id.clone(),
                1_900_000_000_000 + i as u64,
            ))
            .await
            .expect("seed message");
        wall_clock_tick().await;
    }

    bus.emit(EffectEvent::ContextHistorySettled {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
        settlement: baml_rt_core::ContextHistorySettlementKind::ChatStream,
        function_name: None,
    })
    .await
    .expect("emit settled");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head")
            .is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "compaction head not written in time"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn host_ingress_operational_rows_render_for_compaction() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(10, 20);
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            ctx.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1:2:1".to_string(),
            "host.source-records.v1".to_string(),
            2,
            None,
            vec!["1".to_string()],
        ))
        .await
        .expect("poll");
    store
        .add_event(baml_rt_provenance::ProvEvent::message_received_global(
            ctx.clone(),
            MessageId::from_external(ExternalId::new("ingress-user-1")),
            "user".into(),
            vec!["clickup task body".to_string()],
            None,
            provenance_agent_id(),
            1_900_000_000_010,
        ))
        .await
        .expect("ingress user");

    let items = store.conversation_context(&ctx, None).await.expect("items");
    let registry = ToolRegistry::new();
    let ref_table = RefTable::new();
    let rendered = render_items_with_ref_table(&items, &registry, &ref_table);
    assert!(
        rendered.contains("Host event (host.source-records.v1) from slack:slack:C123"),
        "operational poll row must appear in compaction render: {rendered}"
    );
    assert!(
        rendered.contains("clickup task body"),
        "ingress user row must appear: {rendered}"
    );
}

#[tokio::test]
async fn render_items_for_context_includes_operational_only_rows() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(10, 21);
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            ctx.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1:2:2".to_string(),
            "host.source-records.v1".to_string(),
            1,
            None,
            vec!["1".to_string()],
        ))
        .await
        .expect("poll");
    let items = store.conversation_context(&ctx, None).await.expect("items");
    let registry = ToolRegistry::new();
    let rendered = render_items_for_context(&store, &ctx, &items, &registry)
        .await
        .expect("render");
    assert!(
        rendered.contains("Host event (host.source-records.v1) from slack:slack:C123"),
        "operational-only context must render via render_items_for_context: {rendered}"
    );
}

#[tokio::test]
async fn pre_model_emergency_compacts_on_large_prompt() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_900_002);
    let agent_id = provenance_agent_id();
    let subscriber = compaction_subscriber(Arc::clone(&store), agent_id.clone());

    for i in 0..12 {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                ctx.clone(),
                MessageId::from_external(ExternalId::new(format!("emergency-msg-{i}"))),
                "user".into(),
                vec![format!("emergency body {i} {}", "X ".repeat(800))],
                None,
                agent_id.clone(),
                1_900_100_000_000 + i as u64,
            ))
            .await
            .expect("seed message");
        wall_clock_tick().await;
    }

    let items = store.conversation_context(&ctx, None).await.expect("items");
    let prompt_bytes = estimate_compaction_prompt_bytes(&store, &ctx, &items, &ToolRegistry::new())
        .await
        .expect("bytes");
    assert!(
        subscriber.pre_model_exceeds_emergency(prompt_bytes, items.len(), "default"),
        "large prompt must exceed emergency threshold"
    );

    subscriber
        .evaluate_pre_model_emergency(
            &baml_rt_provenance::CompactionRequest {
                context_id: ctx.clone(),
                agent_id: agent_id.clone(),
            },
            prompt_bytes,
            items.len(),
            "default",
        )
        .await;

    assert!(
        store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head")
            .is_some(),
        "pre-model emergency must write compaction head"
    );
}

#[tokio::test]
async fn post_turn_runs_at_settlement_despite_awaiting_input_hint() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_900_003);
    let agent_id = provenance_agent_id();
    let subscriber = compaction_subscriber(Arc::clone(&store), agent_id.clone());

    for i in 0..10 {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                ctx.clone(),
                MessageId::from_external(ExternalId::new(format!("await-msg-{i}"))),
                "user".into(),
                vec![format!("await ping {i} {}", "FILL ".repeat(400))],
                None,
                agent_id.clone(),
                1_900_200_000_000 + i as u64,
            ))
            .await
            .expect("seed message");
        wall_clock_tick().await;
    }

    subscriber
        .evaluate_post_turn(
            &baml_rt_provenance::CompactionRequest {
                context_id: ctx.clone(),
                agent_id: agent_id.clone(),
            },
            "default",
        )
        .await;

    assert!(
        store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head")
            .is_some(),
        "post-turn at settlement must compact even when resume hints exist"
    );
}

#[tokio::test]
async fn planning_agent_prefix_non_empty_with_live_step_in_tail() {
    use baml_rt_llm_config::{BudgetFreshness, BudgetSource, ModelContextBudget};
    let policy = CompactionTriggerPolicy::from_budget(
        ModelContextBudget {
            model_id: "test".into(),
            provider: "test".into(),
            client_name: "test".into(),
            context_window_tokens: 128_000,
            safe_prompt_tokens: 80_000,
            emergency_prompt_tokens: 110_000,
            output_reserve_tokens: 4096,
            source: BudgetSource::Fallback,
            freshness: BudgetFreshness::NotApplicable,
            warning: None,
        },
        4,
        2,
        false,
        false,
    );
    let items = vec![
        msg(10, "old user"),
        planning(20, PlanningEventKind::IntentResolved, "ship feature", None),
        planning(
            30,
            PlanningEventKind::PlanStepStatusChanged,
            "deploy",
            Some("in_progress"),
        ),
        msg(40, "recent"),
        msg(50, "latest"),
    ];
    let range = select_compactable_range(&items, &policy).expect("range");
    let (prefix, tail) = partition_items_for_compaction(&items, &range);
    assert!(!prefix.is_empty(), "sealed intent must stay compactable");
    assert!(
        tail.iter().any(item_is_live_planning_obligation),
        "in-progress step stays in tail"
    );
    assert!(
        !prefix.iter().any(item_is_live_planning_obligation),
        "sealed intent is not a live obligation"
    );
}

fn msg(order: u64, text: &str) -> ProvenanceConversationContextItem {
    ProvenanceConversationContextItem {
        timestamp_ms: order,
        activity_anchor: ActivityAnchorId::from(format!("m{order}")),
        role: "user".into(),
        content: ConversationItemContent::Message {
            text: text.into(),
            citations: vec![],
        },
        user_speaker_kind: None,
    }
}

fn planning(
    order: u64,
    kind: PlanningEventKind,
    summary: &str,
    status: Option<&str>,
) -> ProvenanceConversationContextItem {
    ProvenanceConversationContextItem {
        timestamp_ms: order,
        activity_anchor: ActivityAnchorId::from(format!("p{order}")),
        role: "system".into(),
        content: ConversationItemContent::Planning(PlanningEventContent {
            kind,
            summary: summary.into(),
            detail: None,
            intent_id: Some("i1".into()),
            plan_id: Some("p1".into()),
            step_id: Some("s1".into()),
            old_status: None,
            new_status: status.map(str::to_owned),
        }),
        user_speaker_kind: None,
    }
}
