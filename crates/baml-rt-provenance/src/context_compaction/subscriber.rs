// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Post-turn compaction hook on [`EffectEvent::A2aCompleted`].

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::bus::{EffectEvent, EffectSubscriber, EffectSubscriberTier};
use baml_rt_tools::tools::ToolRegistry;

use super::{
    compactor::ContextCompactionService,
    prepare::prepare_compaction,
    render::wire_history_byte_len,
    summarizer::{ConversationCompactionSummarizer, compaction_runtime_scope},
    trigger::{
        CompactionSafetySignals, CompactionTriggerDecision, CompactionTriggerInput,
        CompactionTriggerPolicy, CompactionTriggerSource, evaluate_compaction_trigger,
    },
    types::{CompactionRequest, ContextCompactionPolicy, ContextCompactionTrigger},
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
    trigger_policy: CompactionTriggerPolicy,
    legacy_policy: ContextCompactionPolicy,
    summarizer: Arc<dyn ConversationCompactionSummarizer>,
    tool_registry: Arc<ToolRegistry>,
}

impl ContextCompactionSubscriber {
    pub fn new(
        store: Arc<SurrealProvenanceStore>,
        writer: Arc<dyn ProvenanceWriter>,
        trigger_policy: CompactionTriggerPolicy,
        legacy_policy: ContextCompactionPolicy,
        summarizer: Arc<dyn ConversationCompactionSummarizer>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            store,
            writer,
            trigger_policy,
            legacy_policy,
            summarizer,
            tool_registry,
        }
    }

    pub fn trigger_policy(&self) -> &CompactionTriggerPolicy {
        &self.trigger_policy
    }

    pub fn legacy_policy(&self) -> &ContextCompactionPolicy {
        &self.legacy_policy
    }

    /// Pre-model emergency evaluation and compaction from projected wire rows.
    pub async fn evaluate_pre_model_from_rows(
        &self,
        request: &CompactionRequest,
        rows: &[serde_json::Value],
        in_flight: bool,
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
        )
        .await;
    }

    /// Manual operator compaction with optional force bypass of threshold checks.
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
        )
        .await;
    }

    /// Evaluate trigger and optionally run compaction.
    pub async fn evaluate_and_compact(
        &self,
        request: &CompactionRequest,
        input: CompactionTriggerInput,
    ) {
        let decision = evaluate_compaction_trigger(&self.trigger_policy, &input);
        let start = std::time::Instant::now();
        let backend = self.summarizer.backend_label();
        let budget = &self.trigger_policy.budget;

        match decision {
            CompactionTriggerDecision::Run(trigger) => {
                self.run_compact(request, trigger, start, backend, budget)
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
    ) {
        let outcome = match self.try_compact_inner(request, trigger).await {
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
    ) -> Result<CompactionAttempt, ()> {
        let context_id = &request.context_id;
        let prepared = match prepare_compaction(
            self.store.as_ref(),
            self.tool_registry.as_ref(),
            &self.legacy_policy,
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
        let safety = resolve_safety_signals(self.store.as_ref(), context_id, None).await;
        self.evaluate_and_compact(
            &request,
            CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count,
                prompt_bytes: 0,
                safety,
                force: false,
            },
        )
        .await;
        Ok(())
    }
}

/// Resolve safety signals from provenance graph (awaiting-input gate).
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
