// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Phase 0: agent prompt read path applies compaction head on read.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::{
    context::RuntimeScope,
    ids::{AgentId, ContextId, ExternalId, MessageId},
};
use baml_rt_llm_config::{LlmClientConfig, ModelBudgetOverride};
use baml_rt_provenance::{
    CompactionPrefixInput, CompactionRequest, CompactionSummarizeError, CompactionTriggerInput,
    CompactionTriggerSource, ContextCompactionSubscriber, ConversationCompactionSummarizer,
    DEFAULT_COMPACTION_ITEM_THRESHOLD, DEFAULT_LLM_CONTEXT_ITEM_CAP, DEFAULT_RECENT_TAIL_RETENTION,
    FixedCompactionSummarizer, ProvenanceContextReader, ProvenanceQueryApi, ProvenanceWriter,
    finalize_compaction_summary, render_items_with_ref_table,
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
    ContextCompactionSubscriber::new(
        store,
        writer,
        Arc::new(llm_config.clone()),
        None,
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
            "default",
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
        !head.summary_text.contains("Preserved refs:"),
        "compaction summary must not inject deterministic ref appendix"
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
            .is_some_and(|c| c.contains("[compaction summary")),
        "first projected row must be compaction summary: {history}"
    );
    assert_eq!(
        rows.len(),
        agent_after_len,
        "every compacted item must project to a transcript row"
    );
}

#[tokio::test]
async fn compaction_rejects_summary_citing_unresolved_ref() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_011);
    let agent_id = provenance_agent_id();
    let message_count = DEFAULT_COMPACTION_ITEM_THRESHOLD + 1;

    seed_user_messages(&store, &ctx, &agent_id, message_count, "status ping @1").await;

    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    let summarizer = Arc::new(FixedCompactionSummarizer::new(
        "User repeatedly asked about status ping @1",
    ));
    let llm_config = LlmClientConfig::sensible_default();
    let subscriber = ContextCompactionSubscriber::new(
        Arc::clone(&store),
        writer,
        Arc::new(llm_config),
        None,
        summarizer,
        Arc::new(ToolRegistry::new()),
    );
    let request = CompactionRequest {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
    };
    subscriber
        .evaluate_and_compact(
            &request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count: message_count,
                prompt_bytes: 0,
                safety: Default::default(),
                force: false,
            },
            "default",
        )
        .await;

    let head = store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head query");
    assert!(
        head.is_none(),
        "summary citing unresolved @1 must not write compaction head"
    );
}

/// Always fails validation until `allow_success` is set (exhausts in-trigger retries each settlement).
struct ValidationGatedSummarizer {
    ok_prose: String,
    allow_success: Arc<std::sync::atomic::AtomicBool>,
}

impl ValidationGatedSummarizer {
    fn new(ok_prose: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            ok_prose: ok_prose.into(),
            allow_success: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
}

#[async_trait]
impl ConversationCompactionSummarizer for ValidationGatedSummarizer {
    fn backend_label(&self) -> &'static str {
        "validation-gated-test"
    }

    async fn summarize_prefix_attempt(
        &self,
        _scope: &RuntimeScope,
        input: &CompactionPrefixInput,
        _validation_feedback: Option<String>,
    ) -> Result<String, CompactionSummarizeError> {
        if !self.allow_success.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CompactionSummarizeError::Validation(
                "compaction summary cites unresolved wire refs: @1".into(),
            ));
        }
        finalize_compaction_summary(&self.ok_prose, &input.ref_table)
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
    }
}

#[tokio::test]
async fn compaction_retries_on_next_settlement_after_validation_failure() {
    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_012);
    let agent_id = provenance_agent_id();
    let message_count = DEFAULT_COMPACTION_ITEM_THRESHOLD + 1;

    seed_user_messages(&store, &ctx, &agent_id, message_count, "status ping").await;

    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    let summarizer =
        ValidationGatedSummarizer::new("User asked about status ping; continue from recent tail.");
    let allow_success = Arc::clone(&summarizer.allow_success);
    let llm_config = LlmClientConfig::sensible_default();
    let subscriber = ContextCompactionSubscriber::new(
        Arc::clone(&store),
        writer,
        Arc::new(llm_config),
        None,
        summarizer,
        Arc::new(ToolRegistry::new()),
    );
    let request = CompactionRequest {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
    };
    let trigger = CompactionTriggerInput {
        source: CompactionTriggerSource::PostTurn,
        item_count: message_count,
        prompt_bytes: 0,
        safety: Default::default(),
        force: false,
    };

    subscriber
        .evaluate_and_compact(&request, trigger, "default")
        .await;
    assert!(
        store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head query")
            .is_none(),
        "first settlement must not write head when validation fails"
    );

    allow_success.store(true, std::sync::atomic::Ordering::SeqCst);
    subscriber
        .evaluate_and_compact(&request, trigger, "default")
        .await;
    let head = store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head query")
        .expect("second settlement should compact after prior failure");
    assert!(
        head.summary_text.contains("[conversation summary]"),
        "head: {:?}",
        head.summary_text
    );
}

#[tokio::test]
async fn compaction_recovers_within_settlement_when_retry_fixes_validation() {
    struct FeedbackThenRecover;

    #[async_trait]
    impl ConversationCompactionSummarizer for FeedbackThenRecover {
        fn backend_label(&self) -> &'static str {
            "feedback-then-recover"
        }

        async fn summarize_prefix_attempt(
            &self,
            _scope: &RuntimeScope,
            input: &CompactionPrefixInput,
            validation_feedback: Option<String>,
        ) -> Result<String, CompactionSummarizeError> {
            if validation_feedback.is_none() {
                return Err(CompactionSummarizeError::Validation(
                    "compaction summary cites unresolved wire refs: @9".into(),
                ));
            }
            finalize_compaction_summary(
                "User asked about status ping; continue from recent tail.",
                &input.ref_table,
            )
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
        }
    }

    let store = build_isolated_store().await;
    let ctx = provenance_context_id(1_800_013);
    let agent_id = provenance_agent_id();
    let message_count = DEFAULT_COMPACTION_ITEM_THRESHOLD + 1;

    seed_user_messages(&store, &ctx, &agent_id, message_count, "status ping").await;

    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    let summarizer = Arc::new(FeedbackThenRecover);
    let subscriber = ContextCompactionSubscriber::new(
        Arc::clone(&store),
        writer,
        Arc::new(LlmClientConfig::sensible_default()),
        None,
        summarizer,
        Arc::new(ToolRegistry::new()),
    );
    let request = CompactionRequest {
        context_id: ctx.clone(),
        agent_id: agent_id.clone(),
    };

    subscriber
        .evaluate_and_compact(
            &request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count: message_count,
                prompt_bytes: 0,
                safety: Default::default(),
                force: false,
            },
            "default",
        )
        .await;

    let head = store
        .latest_compaction_head(&ctx, None)
        .await
        .expect("head query")
        .expect("in-trigger validation retry should compact on first settlement");
    assert!(head.summary_text.contains("[conversation summary]"));
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
        .evaluate_pre_model_from_rows(&request, &rows, false, "default")
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
