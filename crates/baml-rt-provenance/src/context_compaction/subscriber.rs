// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Post-turn compaction hook on [`EffectEvent::A2aCompleted`].

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber, EffectSubscriberTier},
    ids::ContextId,
};
use baml_rt_llm_config::{
    CompactionTriggerPolicy, LlmClientConfig, resolve_compaction_trigger_policy,
};
use baml_rt_tools::tools::ToolRegistry;

use super::{
    compactor::ContextCompactionService,
    context_compaction_policy_from_trigger,
    prepare::prepare_compaction,
    render::{prepare_render_context, wire_history_byte_len},
    summarizer::{ConversationCompactionSummarizer, compaction_runtime_scope},
    trigger::{
        CompactionSafetySignals, CompactionTriggerDecision, CompactionTriggerInput,
        CompactionTriggerSource, evaluate_compaction_trigger,
    },
    types::{CompactionRequest, ContextCompactionTrigger},
};
use crate::{
    store::{ProvenanceContextReader, ProvenanceWriter},
    surreal_store::SurrealProvenanceStore,
};

/// Background subscriber: after each A2A turn, maybe compact sealed transcript prefix.
#[derive(Clone)]
pub struct ContextCompactionSubscriber {
    store: Arc<SurrealProvenanceStore>,
    writer: Arc<dyn ProvenanceWriter>,
    llm_config: Arc<LlmClientConfig>,
    agent_package: Option<String>,
    summarizer: Arc<dyn ConversationCompactionSummarizer>,
    tool_registry: Arc<ToolRegistry>,
}

impl ContextCompactionSubscriber {
    pub fn new(
        store: Arc<SurrealProvenanceStore>,
        writer: Arc<dyn ProvenanceWriter>,
        llm_config: Arc<LlmClientConfig>,
        agent_package: Option<String>,
        summarizer: Arc<dyn ConversationCompactionSummarizer>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            store,
            writer,
            llm_config,
            agent_package,
            summarizer,
            tool_registry,
        }
    }

    #[must_use]
    pub fn trigger_policy_for(&self, function_name: &str) -> CompactionTriggerPolicy {
        resolve_compaction_trigger_policy(
            &self.llm_config,
            self.agent_package.as_deref(),
            function_name,
        )
    }

    #[must_use]
    pub fn pre_model_exceeds_emergency(
        &self,
        rows: &[serde_json::Value],
        function_name: &str,
    ) -> bool {
        let policy = self.trigger_policy_for(function_name);
        let input = CompactionTriggerInput {
            source: CompactionTriggerSource::PreModel,
            item_count: rows.len(),
            prompt_bytes: wire_history_byte_len(rows),
            safety: CompactionSafetySignals::default(),
            force: false,
        };
        matches!(
            evaluate_compaction_trigger(&policy, &input),
            CompactionTriggerDecision::Run(_)
        )
    }

    pub async fn evaluate_pre_model_from_rows(
        &self,
        request: &CompactionRequest,
        rows: &[serde_json::Value],
        in_flight: bool,
        function_name: &str,
    ) {
        let bytes = wire_history_byte_len(rows);
        let safety =
            resolve_safety_signals(self.store.as_ref(), &request.context_id, Some(in_flight)).await;
        self.evaluate_and_compact(
            request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PreModel,
                item_count: rows.len(),
                prompt_bytes: bytes,
                safety,
                force: false,
            },
            function_name,
        )
        .await;
    }

    pub async fn evaluate_manual(&self, request: &CompactionRequest, force: bool, in_flight: bool) {
        let item_count = self
            .store
            .conversation_context(&request.context_id, None)
            .await
            .map(|items| items.len())
            .unwrap_or(0);
        let safety =
            resolve_safety_signals(self.store.as_ref(), &request.context_id, Some(in_flight)).await;
        self.evaluate_and_compact(
            request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::Manual,
                item_count,
                prompt_bytes: 0,
                safety,
                force,
            },
            "default",
        )
        .await;
    }

    pub async fn evaluate_and_compact(
        &self,
        request: &CompactionRequest,
        input: CompactionTriggerInput,
        function_name: &str,
    ) {
        let policy = self.trigger_policy_for(function_name);
        let decision = evaluate_compaction_trigger(&policy, &input);
        let start = std::time::Instant::now();
        let backend = self.summarizer.backend_label();
        let budget = &policy.budget;

        match decision {
            CompactionTriggerDecision::Run(trigger) => {
                self.run_compact(request, trigger, start, backend, budget, function_name)
                    .await;
            }
            CompactionTriggerDecision::Skip(reason) => {
                record_trigger_outcome(TriggerOutcomeRecord {
                    source: input.source,
                    result: "skipped",
                    reason: Some(reason.as_wire_str()),
                    backend,
                    budget,
                    started_at: start,
                    pre_prompt_bytes: 0,
                    post_prompt_bytes: 0,
                    covered_rows: 0,
                });
            }
            CompactionTriggerDecision::Defer(reason) => {
                record_trigger_outcome(TriggerOutcomeRecord {
                    source: input.source,
                    result: "deferred",
                    reason: Some(reason.as_wire_str()),
                    backend,
                    budget,
                    started_at: start,
                    pre_prompt_bytes: 0,
                    post_prompt_bytes: 0,
                    covered_rows: 0,
                });
            }
        }
    }

    async fn run_compact(
        &self,
        request: &CompactionRequest,
        trigger: ContextCompactionTrigger,
        start: std::time::Instant,
        backend: &str,
        budget: &baml_rt_llm_config::ModelContextBudget,
        function_name: &str,
    ) {
        let outcome = match self
            .try_compact_inner(request, trigger, function_name)
            .await
        {
            Ok(CompactionAttempt::Succeeded {
                pre_prompt_bytes,
                post_prompt_bytes,
                covered_rows,
            }) => {
                record_trigger_outcome(TriggerOutcomeRecord {
                    source: trigger_source_from_wire(trigger),
                    result: "success",
                    reason: None,
                    backend,
                    budget,
                    started_at: start,
                    pre_prompt_bytes,
                    post_prompt_bytes,
                    covered_rows,
                });
                return;
            }
            Ok(CompactionAttempt::Skipped) => ("skipped", Some("empty_prefix")),
            Ok(CompactionAttempt::SummarizeFailed) => ("summarize_failed", None),
            Err(_) => ("error", Some("prepare_error")),
        };
        record_trigger_outcome(TriggerOutcomeRecord {
            source: trigger_source_from_wire(trigger),
            result: outcome.0,
            reason: outcome.1,
            backend,
            budget,
            started_at: start,
            pre_prompt_bytes: 0,
            post_prompt_bytes: 0,
            covered_rows: 0,
        });
    }

    async fn try_compact_inner(
        &self,
        request: &CompactionRequest,
        trigger: ContextCompactionTrigger,
        function_name: &str,
    ) -> Result<CompactionAttempt, ()> {
        let context_id = &request.context_id;
        let range_policy =
            context_compaction_policy_from_trigger(&self.trigger_policy_for(function_name));
        let prepared = match prepare_compaction(
            self.store.as_ref(),
            self.tool_registry.as_ref(),
            &range_policy,
            request,
            trigger,
        )
        .await
        {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return Ok(CompactionAttempt::Skipped),
            Err(err) => {
                tracing::warn!(
                    context_id = %context_id,
                    agent_id = %request.agent_id,
                    error = %err,
                    "compaction preparation failed"
                );
                return Err(());
            }
        };

        let scope = compaction_runtime_scope(request);
        let summary = match self
            .summarizer
            .summarize_prefix(&scope, &prepared.input)
            .await
        {
            Ok(summary) => summary,
            Err(err) => {
                tracing::warn!(
                    context_id = %context_id,
                    agent_id = %request.agent_id,
                    error = %err,
                    "compaction summarization failed"
                );
                return Ok(CompactionAttempt::SummarizeFailed);
            }
        };

        let post_prompt_bytes = summary.len() as u64;
        let mut record = prepared.record;
        record.post_prompt_bytes = post_prompt_bytes;
        record.summary_text = summary.clone();
        let event = ContextCompactionService::build_record(record, summary);

        self.writer.add_event(event).await.map_err(|err| {
            tracing::warn!(
                context_id = %context_id,
                error = %err,
                "failed to record compaction provenance event"
            );
        })?;
        Ok(CompactionAttempt::Succeeded {
            pre_prompt_bytes: prepared.pre_prompt_bytes,
            post_prompt_bytes,
            covered_rows: prepared.covered_rows,
        })
    }

    async fn estimate_context_prompt_bytes(&self, context_id: &ContextId) -> u64 {
        let Ok(items) = self.store.conversation_context(context_id, None).await else {
            return 0;
        };
        if items.is_empty() {
            return 0;
        }
        let Ok(Some(render_context)) = prepare_render_context(
            self.store.as_ref(),
            context_id,
            &items,
            self.tool_registry.as_ref(),
        )
        .await
        else {
            return 0;
        };
        render_context
            .render_items(&items, self.tool_registry.as_ref())
            .len() as u64
    }
}

fn trigger_source_from_wire(trigger: ContextCompactionTrigger) -> CompactionTriggerSource {
    match trigger {
        ContextCompactionTrigger::PostTurnThreshold => CompactionTriggerSource::PostTurn,
        ContextCompactionTrigger::PreModelEmergency => CompactionTriggerSource::PreModel,
        ContextCompactionTrigger::ManualOperator => CompactionTriggerSource::Manual,
    }
}

fn trigger_wire_from_source(source: CompactionTriggerSource) -> &'static str {
    match source {
        CompactionTriggerSource::PostTurn => "post_turn_threshold",
        CompactionTriggerSource::PreModel => "pre_model_emergency",
        CompactionTriggerSource::Manual => "manual_operator",
    }
}

struct TriggerOutcomeRecord<'a> {
    source: CompactionTriggerSource,
    result: &'a str,
    reason: Option<&'a str>,
    backend: &'a str,
    budget: &'a baml_rt_llm_config::ModelContextBudget,
    started_at: std::time::Instant,
    pre_prompt_bytes: u64,
    post_prompt_bytes: u64,
    covered_rows: u64,
}

fn record_trigger_outcome(record: TriggerOutcomeRecord<'_>) {
    baml_rt_observability::metrics::record_context_compaction(
        baml_rt_observability::metrics::ContextCompactionMetrics {
            trigger: trigger_wire_from_source(record.source),
            result: record.result,
            reason: record.reason,
            summarizer_backend: record.backend,
            model: &record.budget.model_id,
            provider: &record.budget.provider,
            budget_source: record.budget.source.as_wire_str(),
            budget_freshness: record.budget.freshness.as_wire_str(),
            duration: record.started_at.elapsed(),
            pre_prompt_bytes: record.pre_prompt_bytes,
            post_prompt_bytes: record.post_prompt_bytes,
            covered_rows: record.covered_rows,
        },
    );
}

enum CompactionAttempt {
    Skipped,
    SummarizeFailed,
    Succeeded {
        pre_prompt_bytes: u64,
        post_prompt_bytes: u64,
        covered_rows: u64,
    },
}

#[async_trait]
impl EffectSubscriber for ContextCompactionSubscriber {
    fn name(&self) -> &'static str {
        "context_compaction"
    }

    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Background
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        let EffectEvent::A2aCompleted {
            context_id,
            metadata,
            ..
        } = event
        else {
            return Ok(());
        };
        let request = CompactionRequest {
            context_id: context_id.clone(),
            agent_id: metadata.agent_id.clone(),
        };
        let item_count = self
            .store
            .conversation_context(context_id, None)
            .await
            .map(|items| items.len())
            .unwrap_or(0);
        let prompt_bytes = self.estimate_context_prompt_bytes(context_id).await;
        let safety = resolve_safety_signals(self.store.as_ref(), context_id, None).await;
        self.evaluate_and_compact(
            &request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count,
                prompt_bytes,
                safety,
                force: false,
            },
            "default",
        )
        .await;
        Ok(())
    }
}

pub async fn resolve_safety_signals(
    store: &SurrealProvenanceStore,
    context_id: &baml_rt_core::ids::ContextId,
    in_flight: Option<bool>,
) -> CompactionSafetySignals {
    let hints = crate::resolve_resume_ui_hints(store, context_id.as_str(), None)
        .await
        .unwrap_or_default();
    CompactionSafetySignals {
        in_flight: in_flight.unwrap_or(false),
        awaiting_input: hints.awaiting_input,
    }
}
