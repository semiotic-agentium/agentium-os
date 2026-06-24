// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Phase 0: agent prompt read path applies compaction head on read.

use std::sync::Arc;

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId};
use baml_rt_llm_config::{LlmClientConfig, ModelBudgetOverride};
use baml_rt_provenance::{
    CompactionRequest, CompactionTriggerInput, CompactionTriggerSource,
    ContextCompactionSubscriber, DEFAULT_COMPACTION_ITEM_THRESHOLD, DEFAULT_LLM_CONTEXT_ITEM_CAP,
    DEFAULT_RECENT_TAIL_RETENTION, FixedCompactionSummarizer, ProvenanceContextReader,
    ProvenanceQueryApi, ProvenanceWriter, render_items_with_ref_table, resolve_compaction_policies,
};
use baml_rt_tools::{
    archive_refs::RefTable, prompt_projection::project_prompt_context, tools::ToolRegistry,
};
use test_support::testing::provenance_fixtures::{
    build_isolated_store, provenance_agent_id, provenance_context_id, wall_clock_tick,
};

async fn seed_user_messages(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    agent_id: &AgentId,
    count: usize,
    body: &str,
) {
    for i in 0..count {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                context_id.clone(),
                MessageId::from_external(ExternalId::new(format!("compact-msg-{i}"))),
                "user".into(),
                vec![body.to_string()],
                None,
                agent_id.clone(),
                1_800_000_000_000 + i as u64,
            ))
            .await
            .expect("message_received");
        wall_clock_tick().await;
    }
}

fn projected_transcript_bytes(items: &[ProvenanceConversationContextItem]) -> usize {
    let registry = ToolRegistry::new();
    let ref_table = RefTable::new();
    render_items_with_ref_table(items, &registry, &ref_table).len()
}

fn compaction_subscriber(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    agent_id: AgentId,
    llm_config: &LlmClientConfig,
) -> ContextCompactionSubscriber {
    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    let summarizer = Arc::new(FixedCompactionSummarizer::new(format!(
        "Compacted history for agent {agent_id}; user asked about status pings."
    )));
    let (trigger_policy, legacy_policy) = resolve_compaction_policies(llm_config, None, "default");
    ContextCompactionSubscriber::new(
        store,
        writer,
        trigger_policy,
        legacy_policy,
        summarizer,
        Arc::new(ToolRegistry::new()),
    )
}

#[tokio::test]
async fn agent_prompt_read_applies_compaction_head_after_write() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_001);
    let agent_id = provenance_agent_id();
    let message_count = DEFAULT_COMPACTION_ITEM_THRESHOLD + 1;

    seed_user_messages(&store, &ctx, &agent_id, message_count, "status ping @1").await;

    let full_before = store.conversation_context(&ctx, None).await.expect("full");
    let agent_before = store
        .conversation_context_for_agent_prompt(&ctx, None, None)
        .await
        .expect("agent");
    assert_eq!(
        full_before.len(),
        message_count,
        "full transcript includes every seeded message"
    );
    assert!(
        agent_before.len() <= DEFAULT_LLM_CONTEXT_ITEM_CAP,
        "agent read is capped before compaction"
    );

    let request = CompactionRequest {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
    };
    let llm_config = LlmClientConfig::sensible_default();
    compaction_subscriber(Arc::clone(&store), agent_id.clone(), &llm_config)
        .evaluate_and_compact(
            &request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count: message_count,
                prompt_bytes: 0,
                safety: Default::default(),
                force: false,
            },
        )
        .await;

    let head = store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head")
        .expect("compaction head written");
    assert!(
        head.summary_text.contains("[conversation summary]"),
        "compaction summary uses wire format"
    );
    assert!(
        head.summary_text.contains("Preserved refs:"),
        "compaction summary includes deterministic ref appendix"
    );
    assert!(
        head.summary_text.contains("@1"),
        "compaction summary preserves @1 from source"
    );

    let full_after = store.conversation_context(&ctx, None).await.expect("full");
    let agent_after = store
        .conversation_context_for_agent_prompt(&ctx, None, None)
        .await
        .expect("agent");
    let query_after = store
        .query_conversation_context(&ctx, None, None, None)
        .await
        .expect("query");

    assert_eq!(
        agent_after.len(),
        query_after.len(),
        "query API matches agent prompt read row count"
    );
    assert_eq!(
        full_after.len(),
        full_before.len(),
        "full graph read is unchanged after compaction"
    );
    assert!(
        agent_after.len() < full_after.len(),
        "compacted agent view must drop covered prefix rows"
    );
    assert!(
        agent_after.len() <= DEFAULT_RECENT_TAIL_RETENTION + 1,
        "agent view is summary plus recent tail"
    );

    let has_summary = agent_after.iter().any(|item| {
        matches!(
            item.content,
            ConversationItemContent::CompactionSummary { .. }
        )
    });
    assert!(
        has_summary,
        "agent prompt must include compaction summary row"
    );

    let agent_after_len = agent_after.len();
    let projection_items: Vec<_> = agent_after
        .into_iter()
        .filter_map(baml_rt_conversation::provenance_item_to_projection_item)
        .collect();
    let ref_table = Arc::new(RefTable::new());
    let history = project_prompt_context(projection_items, &ToolRegistry::new(), &ref_table, None);
    let rows = history.as_array().expect("history array");
    assert!(
        rows.first()
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("[compaction summary") && c.contains("Preserved refs:")),
        "first projected row must be compaction summary: {history}"
    );
    assert_eq!(
        rows.len(),
        agent_after_len,
        "every compacted item must project to a transcript row"
    );
}

#[tokio::test]
async fn pre_model_emergency_trigger_compacts_large_agent_prompt() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_002);
    let agent_id = provenance_agent_id();
    let large_body = "x".repeat(900);

    seed_user_messages(
        &store,
        &ctx,
        &agent_id,
        DEFAULT_COMPACTION_ITEM_THRESHOLD + 1,
        &large_body,
    )
    .await;

    let mut llm_config = LlmClientConfig::sensible_default();
    llm_config.compaction.client_overrides.insert(
        "OpenRouter".to_string(),
        ModelBudgetOverride {
            context_window_tokens: Some(8_192),
            trigger_ratio: Some(0.5),
            emergency_ratio: Some(0.75),
            output_reserve_tokens: Some(512),
        },
    );

    let agent_before = store
        .conversation_context_for_agent_prompt(&ctx, None, None)
        .await
        .expect("agent");
    let bytes_before = projected_transcript_bytes(&agent_before);
    let emergency_bytes = llm_config
        .compaction
        .client_overrides
        .get("OpenRouter")
        .and_then(|o| o.context_window_tokens)
        .map(|cw| {
            let usable = cw.saturating_sub(512);
            ((usable as f64) * 0.75) as u64 * 4
        })
        .unwrap_or(32_768);
    assert!(
        bytes_before as u64 >= emergency_bytes,
        "fixture must exceed emergency prompt threshold (bytes={bytes_before}, threshold={emergency_bytes})"
    );

    let request = CompactionRequest {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
    };
    let subscriber = compaction_subscriber(Arc::clone(&store), agent_id, &llm_config);
    let projection_items: Vec<_> = agent_before
        .iter()
        .cloned()
        .filter_map(baml_rt_conversation::provenance_item_to_projection_item)
        .collect();
    let ref_table = Arc::new(RefTable::new());
    let history = project_prompt_context(projection_items, &ToolRegistry::new(), &ref_table, None);
    let rows = history.as_array().cloned().unwrap_or_default();
    subscriber
        .evaluate_pre_model_from_rows(&request, &rows, false)
        .await;

    store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head")
        .expect("emergency compaction head");

    let agent_after = store
        .conversation_context_for_agent_prompt(&ctx, None, None)
        .await
        .expect("agent");
    let bytes_after = projected_transcript_bytes(&agent_after);
    assert!(
        bytes_after < bytes_before,
        "emergency compaction must shrink projected prompt (before={bytes_before}, after={bytes_after})"
    );
}
