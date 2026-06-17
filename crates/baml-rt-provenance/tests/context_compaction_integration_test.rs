// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Phase 0: agent prompt read path applies compaction head on read.

use std::sync::Arc;

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId};
use baml_rt_provenance::{
    ContextCompactionPolicy, ContextCompactionSubscriber, ContextCompactionTrigger,
    DEFAULT_COMPACTION_ITEM_THRESHOLD, DEFAULT_LLM_CONTEXT_ITEM_CAP, DEFAULT_RECENT_TAIL_RETENTION,
    ProvenanceContextReader, ProvenanceQueryApi, ProvenanceWriter,
};
use baml_rt_tools::{
    archive_refs::RefTable,
    prompt_projection::{PromptProjectionItem, project_prompt_context},
    tools::ToolRegistry,
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
    let projection_items: Vec<PromptProjectionItem> = items
        .iter()
        .cloned()
        .filter_map(baml_rt_conversation::provenance_item_to_projection_item)
        .collect();
    let ref_table = Arc::new(RefTable::new());
    let registry = ToolRegistry::new();
    let history = project_prompt_context(projection_items, &registry, &ref_table, None);
    baml_rt_tools::prompt_projection::format_conversation_history_transcript(
        history.as_array().unwrap_or(&vec![]),
    )
    .len()
}

fn compaction_subscriber(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
) -> ContextCompactionSubscriber {
    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    ContextCompactionSubscriber::new(store, writer, ContextCompactionPolicy::default())
}

#[tokio::test]
async fn agent_prompt_read_applies_compaction_head_after_write() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_001);
    let agent_id = provenance_agent_id();
    let message_count = DEFAULT_COMPACTION_ITEM_THRESHOLD + 1;

    seed_user_messages(&store, &ctx, &agent_id, message_count, "status ping").await;

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
    assert!(
        agent_before.len() < full_before.len() || message_count <= DEFAULT_LLM_CONTEXT_ITEM_CAP,
        "long transcripts are tail-capped on read before compaction"
    );

    compaction_subscriber(Arc::clone(&store))
        .try_compact(&ctx, ContextCompactionTrigger::PostTurnThreshold)
        .await;

    let head = store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head")
        .expect("compaction head written");
    assert!(
        !head.summary_text.is_empty(),
        "compaction summary must be non-empty"
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
    assert!(
        agent_after.len() < agent_before.len(),
        "compacted row count must shrink versus pre-compaction agent read"
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
    assert!(
        rows.len() <= DEFAULT_RECENT_TAIL_RETENTION + 1,
        "projected history is summary plus recent tail"
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

    let agent_before = store
        .conversation_context_for_agent_prompt(&ctx, None, None)
        .await
        .expect("agent");
    let bytes_before = projected_transcript_bytes(&agent_before);
    assert!(
        bytes_before >= baml_rt_provenance::DEFAULT_COMPACTION_PROMPT_BYTES_THRESHOLD as usize,
        "fixture must exceed emergency prompt threshold (bytes={bytes_before})"
    );

    compaction_subscriber(Arc::clone(&store))
        .try_compact(&ctx, ContextCompactionTrigger::PreModelEmergency)
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
