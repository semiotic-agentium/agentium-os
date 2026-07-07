// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Provenance subscriber: converts EffectEvent to ProvEvent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber, EffectSubscriberTier},
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
    llm_text::{extract_response_text, llm_completion_should_materialize_assistant_message},
};
use baml_rt_observability::metrics::{self, LlmCallMetrics};
use baml_rt_semiotic::{
    CitationIntegrityAssessment, CitationIntegrityEntry, IntegrityStatus, resolve_semiotic_policy,
};
use baml_rt_tools::{
    ToolRegistry,
    archive_refs::RefTable,
    citations::{ParsedCitation, ResolvedCitation},
    prompt_projection::project_prompt_context,
};
use serde_json::Value;

use crate::{
    events::{
        BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR, CallScope, LlmUsage, PlanStepSpec, ProvEvent,
        ProvEventData, ResolvedCitationTarget,
    },
    id_semantics::{SessionStepEntityId, SessionStepEntityInput},
    provenance_item_to_projection_item,
    store::ProvenanceWriter,
    types::ProvEntityId,
};

/// Event type for provenance event construction
#[derive(Debug, Clone, Copy)]
enum ProvenanceEventType {
    ToolCall,
    LlmCall,
}

impl ProvenanceEventType {
    fn as_str(self) -> &'static str {
        match self {
            ProvenanceEventType::ToolCall => "Tool call",
            ProvenanceEventType::LlmCall => "LLM call",
        }
    }
}

/// Extract `citations: string[]` from a BAML LLM result (top-level or under `step`).
fn extract_citation_strings_from_llm_result(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = payload.get("citations").and_then(Value::as_array) {
        for v in arr {
            if let Some(s) = v.as_str() {
                out.push(s.to_string());
            }
        }
    }
    if let Some(step) = payload.get("step").and_then(Value::as_object)
        && let Some(arr) = step.get("citations").and_then(Value::as_array)
    {
        for v in arr {
            if let Some(s) = v.as_str() {
                out.push(s.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Helper to build provenance events with task/global branching.
fn build_prov_event<F, G>(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
    build_task: F,
    build_global: G,
) -> baml_rt_core::Result<ProvEvent>
where
    F: FnOnce(ContextId, TaskId) -> ProvEvent,
    G: FnOnce(ContextId, MessageId) -> ProvEvent,
{
    Ok(match resolve_call_scope(context_id, metadata, event_type) {
        CallScopeResolution::Task(task_id) => build_task(context_id.clone(), task_id),
        CallScopeResolution::Message(message_id) => build_global(context_id.clone(), message_id),
    })
}

/// Helper for completion events — always emits, using a synthetic message_id
/// fallback when both task_id and message_id are absent from metadata.
fn build_prov_event_completion<F, G>(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
    build_task: F,
    build_global: G,
) -> Option<ProvEvent>
where
    F: FnOnce(ContextId, TaskId) -> ProvEvent,
    G: FnOnce(ContextId, MessageId) -> ProvEvent,
{
    Some(match resolve_call_scope(context_id, metadata, event_type) {
        CallScopeResolution::Task(task_id) => build_task(context_id.clone(), task_id),
        CallScopeResolution::Message(message_id) => build_global(context_id.clone(), message_id),
    })
}

enum CallScopeResolution {
    Task(TaskId),
    Message(MessageId),
}

fn synthetic_context_message_id(context_id: &ContextId) -> MessageId {
    MessageId::from_external(ExternalId::new(format!("ctx-msg:{}", context_id.as_str())))
}

fn resolve_call_scope(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
) -> CallScopeResolution {
    if let Some(task_id) = task_id_from_metadata(metadata) {
        return CallScopeResolution::Task(task_id);
    }
    let mut message_id = message_id_from_metadata(metadata);
    if message_id.is_none() {
        tracing::error!(
            event_type = event_type.as_str(),
            context_id = %context_id,
            "effect metadata missing both task_id and message_id; using synthetic fallback"
        );
        message_id = Some(synthetic_context_message_id(context_id));
    }
    CallScopeResolution::Message(message_id.expect("synthetic fallback always set"))
}

/// Adapter that subscribes to effect events and emits provenance events.
pub type ActionDescriber = dyn Fn(Option<&str>, &serde_json::Value) -> Option<String> + Send + Sync;

pub struct ProvenanceEffectSubscriber {
    writer: Arc<dyn ProvenanceWriter>,
    action_describer: Option<Arc<ActionDescriber>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    provenance_store: Option<Arc<crate::surreal_store::SurrealProvenanceStore>>,
    archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
}

impl ProvenanceEffectSubscriber {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            writer,
            action_describer: None,
            tool_registry: None,
            provenance_store: None,
            archive_ref_tables: None,
        }
    }

    pub fn set_action_describer(&mut self, describer: Arc<ActionDescriber>) {
        self.action_describer = Some(describer);
    }

    pub fn set_tool_registry(&mut self, registry: Arc<ToolRegistry>) {
        self.tool_registry = Some(registry);
    }

    pub fn set_provenance_store(
        &mut self,
        store: Arc<crate::surreal_store::SurrealProvenanceStore>,
    ) {
        self.provenance_store = Some(store);
    }

    pub fn set_archive_ref_tables(
        &mut self,
        tables: Arc<baml_rt_tools::archive_refs::ContextRefTables>,
    ) {
        self.archive_ref_tables = Some(tables);
    }

    fn invalidate_context_ref_cache(&self, context_id: &ContextId) {
        if let Some(tables) = &self.archive_ref_tables {
            baml_rt_tools::archive_refs::invalidate_ref_table(tables, context_id.as_str());
        }
    }

    async fn ref_table_for_context(
        &self,
        context_id: &ContextId,
        conversation_items: &[ProvenanceConversationContextItem],
    ) -> Option<Arc<RefTable>> {
        let registry = self.tool_registry.as_ref()?;
        let projection_items: Vec<_> = conversation_items
            .iter()
            .cloned()
            .filter_map(provenance_item_to_projection_item)
            .collect();
        if let Some(store) = self.provenance_store.as_ref() {
            crate::surreal_store::prepare_ref_table_for_projection(
                store,
                context_id,
                &projection_items,
                registry.as_ref(),
            )
            .await
            .ok()
        } else {
            Some(
                self.archive_ref_tables
                    .as_ref()
                    .and_then(|tables| {
                        baml_rt_tools::archive_refs::get_ref_table(tables, context_id.as_str())
                    })
                    .unwrap_or_else(|| Arc::new(RefTable::new())),
            )
        }
    }

    async fn compute_citation_integrity(
        &self,
        context_id: &ContextId,
        citation_strings: &[String],
        conversation_items: &[ProvenanceConversationContextItem],
        strict_citation_anchors: bool,
    ) -> Option<CitationIntegrityAssessment> {
        if citation_strings.is_empty() {
            return None;
        }
        let ref_table = self
            .ref_table_for_context(context_id, conversation_items)
            .await?;
        let _history = project_prompt_context(
            conversation_items
                .iter()
                .cloned()
                .filter_map(provenance_item_to_projection_item)
                .collect(),
            self.tool_registry.as_ref()?,
            &ref_table,
            None,
        );

        let mut per_citation = Vec::new();
        let mut resolved_count = 0u32;
        let mut unresolved_count = 0u32;

        for raw in citation_strings {
            let parsed = match ParsedCitation::parse(raw) {
                Ok(c) => c,
                Err(_) => {
                    unresolved_count += 1;
                    per_citation.push(CitationIntegrityEntry {
                        raw: raw.clone(),
                        n: 0,
                        is_history: false,
                        negated: false,
                        status: IntegrityStatus::Unresolved,
                        activity_anchor: None,
                        content_preview: None,
                    });
                    continue;
                }
            };
            let negated = matches!(
                &parsed,
                ParsedCitation::History { negated: true, .. }
                    | ParsedCitation::Archive { negated: true, .. }
            );
            let is_history = matches!(parsed, ParsedCitation::History { .. });
            let n = match &parsed {
                ParsedCitation::History { n, .. } => *n,
                ParsedCitation::Archive { local, .. } => *local,
            };

            if let Some(resolved) = ResolvedCitation::resolve(&parsed, &ref_table) {
                let status = if negated {
                    IntegrityStatus::Negated
                } else {
                    IntegrityStatus::Resolved
                };
                if matches!(status, IntegrityStatus::Resolved) {
                    resolved_count += 1;
                }
                let content_preview = if resolved.content.len() > 400 {
                    Some(format!("{}…", &resolved.content[..400]))
                } else {
                    Some(resolved.content.clone())
                };
                per_citation.push(CitationIntegrityEntry {
                    raw: raw.clone(),
                    n,
                    is_history,
                    negated,
                    status,
                    activity_anchor: Some(resolved.activity_anchor),
                    content_preview,
                });
            } else {
                unresolved_count += 1;
                per_citation.push(CitationIntegrityEntry {
                    raw: raw.clone(),
                    n,
                    is_history,
                    negated,
                    status: IntegrityStatus::Unresolved,
                    activity_anchor: None,
                    content_preview: None,
                });
            }
        }

        if per_citation.is_empty() {
            return None;
        }

        Some(CitationIntegrityAssessment {
            per_citation,
            unresolved_count,
            resolved_count,
            strict_mode: strict_citation_anchors,
            strict_violation: strict_citation_anchors && unresolved_count > 0,
        })
    }

    fn extract_resolved_citations(
        integrity: &Option<CitationIntegrityAssessment>,
        conversation_items: &[ProvenanceConversationContextItem],
    ) -> Vec<ResolvedCitationTarget> {
        let Some(assessment) = integrity.as_ref() else {
            return vec![];
        };

        let mut anchor_to_node_id: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for item in conversation_items {
            let anchor = item.activity_anchor.as_str();
            if let ConversationItemContent::SessionStep(_) = &item.content {
                let node_id =
                    ProvEntityId::derived::<SessionStepEntityId>(SessionStepEntityInput {
                        event_anchor: anchor,
                    })
                    .into_string();
                anchor_to_node_id.insert(anchor.to_string(), node_id);
            }
        }

        assessment
            .per_citation
            .iter()
            .filter(|c| {
                matches!(
                    c.status,
                    IntegrityStatus::Resolved | IntegrityStatus::Negated
                )
            })
            .filter_map(|c| {
                let activity_anchor = c.activity_anchor.as_deref()?;
                if c.is_history {
                    return None;
                }
                let node_id = anchor_to_node_id.get(activity_anchor)?;
                Some(ResolvedCitationTarget {
                    target_node_id: node_id.clone(),
                    raw: c.raw.clone(),
                    line_start: None,
                    line_end: None,
                    negated: c.negated,
                })
            })
            .collect()
    }
}

#[async_trait]
impl EffectSubscriber for ProvenanceEffectSubscriber {
    fn name(&self) -> &'static str {
        "provenance"
    }

    /// Awaited because the next `conversation_context` read structurally
    /// depends on this subscriber's Surreal write being committed.
    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Awaitable
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        let prov_event = match event {
            EffectEvent::ToolStarted {
                context_id,
                metadata,
            } => build_prov_event(
                context_id,
                &metadata.metadata,
                ProvenanceEventType::ToolCall,
                |ctx_id, task_id| {
                    ProvEvent::tool_call_started_task(
                        ctx_id,
                        task_id,
                        metadata.tool_name.clone(),
                        metadata.function_name.clone(),
                        metadata.args.clone(),
                        metadata.metadata.clone(),
                        metadata.delegation_target.clone(),
                    )
                    .with_tool_backend_digest(
                        metadata.tool_backend.clone(),
                        metadata.tool_digest.clone(),
                    )
                },
                |ctx_id, msg_id| {
                    ProvEvent::tool_call_started_global(
                        ctx_id,
                        msg_id,
                        metadata.tool_name.clone(),
                        metadata.function_name.clone(),
                        metadata.args.clone(),
                        metadata.metadata.clone(),
                        metadata.delegation_target.clone(),
                    )
                    .with_tool_backend_digest(
                        metadata.tool_backend.clone(),
                        metadata.tool_digest.clone(),
                    )
                },
            )?,
            EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                outcome,
                result,
            } => {
                // Merge the result (if any) into metadata so the provenance store can write
                // it to the tool_result payload. Reserved anchor (if present) is consumed for
                // event-id assignment and removed from persisted metadata.
                let mut map = match &metadata.metadata {
                    serde_json::Value::Object(m) => m.clone(),
                    _ => serde_json::Map::new(),
                };
                let reserved_anchor = map
                    .get(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ActivityAnchorId::from);
                map.remove(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR);
                if let Some(result_value) = result {
                    map.insert("result".to_string(), result_value.clone());
                }
                let enriched_metadata = serde_json::Value::Object(map);
                let event = match build_prov_event_completion(
                    context_id,
                    &enriched_metadata,
                    ProvenanceEventType::ToolCall,
                    |ctx_id, task_id| {
                        let event = if let Some(id) = reserved_anchor.clone() {
                            ProvEvent::tool_call_completed_task_with_id(
                                id,
                                ctx_id,
                                task_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        } else {
                            ProvEvent::tool_call_completed_task(
                                ctx_id,
                                task_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        };
                        event.with_tool_backend_digest(
                            metadata.tool_backend.clone(),
                            metadata.tool_digest.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        let event = if let Some(id) = reserved_anchor.clone() {
                            ProvEvent::tool_call_completed_global_with_id(
                                id,
                                ctx_id,
                                msg_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        } else {
                            ProvEvent::tool_call_completed_global(
                                ctx_id,
                                msg_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        };
                        event.with_tool_backend_digest(
                            metadata.tool_backend.clone(),
                            metadata.tool_digest.clone(),
                        )
                    },
                ) {
                    Some(event) => event,
                    None => return Ok(()), // Skip on missing message_id
                };
                tracing::debug!(
                    event = "provenance_emit",
                    source = "effect_subscriber.tool_completion",
                    prov_event_id = %event.id(),
                    tool_name = %metadata.tool_name,
                    function_name = ?metadata.function_name,
                    context_id = %context_id,
                    task_id = ?task_id_from_metadata(&metadata.metadata),
                    "Emitting tool completion provenance event from effect-subscriber path"
                );
                event
            }
            EffectEvent::LlmStarted {
                context_id,
                metadata,
            } => {
                let prompt = normalized_prompt(&metadata.prompt);
                build_prov_event(
                    context_id,
                    &metadata.metadata,
                    ProvenanceEventType::LlmCall,
                    |ctx_id, task_id| {
                        ProvEvent::llm_call_started_task(
                            ctx_id,
                            task_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_started_global(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                        )
                    },
                )?
            }
            EffectEvent::LlmCompleted {
                context_id,
                metadata,
                usage,
                result_payload,
                duration_ms,
                outcome,
                rejection_reason,
            } => {
                let prov_usage = match usage {
                    Some(baml_rt_core::bus::LlmUsage::Known {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_input_tokens,
                    }) => LlmUsage::Known {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        total_tokens: *total_tokens,
                        cached_input_tokens: *cached_input_tokens,
                    },
                    Some(baml_rt_core::bus::LlmUsage::Unknown) | None => LlmUsage::Unknown,
                };
                let prov_usage_clone = prov_usage.clone();
                let prompt = normalized_prompt(&metadata.prompt);
                let task_id = task_id_from_metadata(&metadata.metadata);
                let result_label = if bool::from(*outcome) {
                    "success"
                } else {
                    "error"
                };
                let (tokens_in, tokens_out) = usage_tokens(&prov_usage);
                let citation_strings = result_payload
                    .as_ref()
                    .map(extract_citation_strings_from_llm_result)
                    .unwrap_or_default();
                // Single store read for citation-grounded drift + resolved-citation extraction.
                // This avoids duplicate conversation_context_with_task reads on the LlmCompleted hot path.
                let conv_items_for_citations = if citation_strings.is_empty() {
                    Vec::new()
                } else {
                    self.writer
                        .conversation_context_with_task(context_id, Some(320), task_id.as_ref())
                        .await
                        .unwrap_or_default()
                };
                let agent_package = metadata
                    .metadata
                    .get("agent_package")
                    .and_then(Value::as_str);
                let citation_policy = resolve_semiotic_policy(agent_package);
                let citation_integrity = if bool::from(*outcome) && !citation_strings.is_empty() {
                    self.compute_citation_integrity(
                        context_id,
                        &citation_strings,
                        &conv_items_for_citations,
                        citation_policy.strict_citation_anchors,
                    )
                    .await
                } else {
                    None
                };
                if let Some(ref integrity) = citation_integrity
                    && integrity.strict_violation
                {
                    tracing::warn!(
                        context_id = %context_id,
                        unresolved = integrity.unresolved_count,
                        agent_package = ?agent_package,
                        "strict citation anchors: unresolved refs in LLM output"
                    );
                }
                let resolved_citations = Self::extract_resolved_citations(
                    &citation_integrity,
                    &conv_items_for_citations,
                );
                let completion_metadata = match &metadata.metadata {
                    Value::Object(map) => {
                        let mut out = map.clone();
                        if let Some(result_payload) = result_payload.clone() {
                            out.insert("result".to_string(), result_payload);
                        }
                        Value::Object(out)
                    }
                    _ => metadata.metadata.clone(),
                };
                let Some(completed_event) = build_prov_event_completion(
                    context_id,
                    &completion_metadata,
                    ProvenanceEventType::LlmCall,
                    |ctx_id, task_id| {
                        ProvEvent::llm_call_completed_task_with_integrity(
                            ctx_id,
                            task_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            completion_metadata.clone(),
                            prov_usage.clone(),
                            *duration_ms,
                            *outcome,
                            citation_integrity.clone(),
                            citation_strings.clone(),
                            resolved_citations.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_completed_global_with_integrity(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            completion_metadata.clone(),
                            prov_usage_clone,
                            *duration_ms,
                            *outcome,
                            citation_integrity.clone(),
                            citation_strings.clone(),
                            resolved_citations.clone(),
                        )
                    },
                ) else {
                    return Ok(()); // Skip on missing message_id
                };
                let prompt_bytes_for_metrics = match completed_event.data() {
                    ProvEventData::LlmCallCompleted {
                        prompt_serialized_utf8_bytes,
                        ..
                    } => *prompt_serialized_utf8_bytes as usize,
                    _ => 0,
                };
                metrics::record_llm_call(&LlmCallMetrics {
                    function_name: &metadata.function_name,
                    client: &metadata.client,
                    model: &metadata.model,
                    result: result_label,
                    duration: std::time::Duration::from_millis(*duration_ms),
                    prompt_bytes: prompt_bytes_for_metrics,
                    tokens_in,
                    tokens_out,
                });
                let completed_id = completed_event.id().clone();
                let client_alias = metadata
                    .metadata
                    .get("client_alias")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let model_alias = metadata
                    .metadata
                    .get("model_alias")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                tracing::debug!(
                    event = "provenance_emit",
                    source = "effect_subscriber.llm_completion",
                    prov_event_id = %completed_id,
                    function_name = %metadata.function_name,
                    client = %metadata.client,
                    model = %metadata.model,
                    client_alias = client_alias,
                    model_alias = model_alias,
                    context_id = %context_id,
                    task_id = ?task_id,
                    citations_count = citation_strings.len(),
                    has_citation_integrity = citation_integrity.is_some(),
                    "Emitting LLM completion provenance event from effect-subscriber path"
                );
                let citation_values = resolved_citations
                    .iter()
                    .filter_map(|t| baml_rt_core::Citation::try_new(&t.raw).ok())
                    .collect::<Vec<_>>();
                self.writer
                    .add_event_with_logging(completed_event, "effect subscriber")
                    .await;
                if bool::from(*outcome)
                    && llm_completion_should_materialize_assistant_message(result_payload.as_ref())
                {
                    emit_llm_assistant_transcript_message(
                        self.writer.as_ref(),
                        context_id,
                        &completion_metadata,
                        result_payload.as_ref(),
                        &completed_id,
                        *duration_ms,
                        citation_values,
                    )
                    .await;
                }
                self.invalidate_context_ref_cache(context_id);
                if !bool::from(*outcome) && rejection_reason.as_deref().is_some() {
                    let reason = rejection_reason.clone().unwrap_or_default();
                    tracing::warn!(
                        reason = %reason,
                        "Prompt output rejected; emitting PromptRejected in provenance"
                    );
                    let rejected_event = build_prov_event(
                        context_id,
                        &metadata.metadata,
                        ProvenanceEventType::LlmCall,
                        |ctx_id, task_id| {
                            ProvEvent::prompt_rejected_task(
                                ctx_id,
                                task_id,
                                completed_id.clone(),
                                reason.clone(),
                            )
                        },
                        |ctx_id, msg_id| {
                            ProvEvent::prompt_rejected_global(
                                ctx_id,
                                msg_id,
                                completed_id.clone(),
                                reason.clone(),
                            )
                        },
                    )?;
                    self.writer
                        .add_event_with_logging(rejected_event, "effect subscriber")
                        .await;
                    self.invalidate_context_ref_cache(context_id);
                }
                return Ok(());
            }
            // A2A effects are primarily for liveness gating, not provenance
            // Skip provenance emission for A2A lifecycle events.
            EffectEvent::A2aStarted { .. }
            | EffectEvent::A2aCompleted { .. }
            | EffectEvent::ContextHistorySettled { .. } => {
                return Ok(());
            }
            EffectEvent::IntentResolved {
                context_id,
                task_id,
                intent_id,
                description,
                citations,
                supersession,
                epoch: _,
            } => ProvEvent::intent_resolved(
                context_id.clone(),
                task_id.clone(),
                intent_id.clone(),
                description.clone(),
                citations.clone(),
                *supersession,
                None,
            ),
            EffectEvent::PlanGenerated {
                context_id,
                task_id,
                intent_id,
                plan_id,
                steps,
                supersession,
                epoch: _,
            } => {
                let steps: Vec<PlanStepSpec> =
                    serde_json::from_value(steps.clone()).map_err(|e| {
                        baml_rt_core::BamlRtError::InvalidArgument(format!(
                            "plan generated effect steps must decode as PlanStepSpec[]: {e}"
                        ))
                    })?;

                ProvEvent::plan_generated(
                    context_id.clone(),
                    task_id.clone(),
                    intent_id.clone(),
                    plan_id.clone(),
                    steps,
                    *supersession,
                )
            }
            EffectEvent::PlanStepStatusChanged {
                context_id,
                task_id,
                intent_id,
                plan_id,
                step_id,
                old_status,
                new_status,
                citations,
                epoch: _,
            } => ProvEvent::plan_step_status_changed(
                context_id.clone(),
                task_id.clone(),
                intent_id.clone(),
                plan_id.clone(),
                step_id.clone(),
                old_status.clone(),
                new_status.clone(),
                citations.clone(),
            ),
            // Tool stream chunks are relay-only; tools are already recorded via the tool interceptor
            EffectEvent::ToolStreamChunk { .. } => return Ok(()),
            EffectEvent::ToolSessionStep {
                context_id,
                tool_name,
                session_id,
                op,
                task_id,
            } => {
                // Task-scoped runs: tie session steps to the task so task-filtered episode
                // transcripts include Open / SendDone / SearchRead / PageRead rows. Otherwise fall back to
                // message scope (synthetic id when the context has no messages yet).
                let scope = if let Some(tid) = task_id {
                    CallScope::Task {
                        task_id: tid.clone(),
                    }
                } else {
                    self.writer
                        .context_messages(context_id, Some(1))
                        .await
                        .ok()
                        .and_then(|msgs| msgs.into_iter().next())
                        .map(|m| CallScope::Message {
                            message_id: m.message_id,
                        })
                        .unwrap_or_else(|| CallScope::Message {
                            message_id: synthetic_context_message_id(context_id),
                        })
                };
                ProvEvent::tool_session_step(
                    context_id.clone(),
                    scope,
                    tool_name.clone(),
                    session_id.clone(),
                    op,
                )
            }
        };

        let ctx_for_cache = prov_event.context_id_opt().cloned();
        self.writer
            .add_event_with_logging(prov_event, "effect subscriber")
            .await;
        if let Some(ctx) = ctx_for_cache.as_ref() {
            self.invalidate_context_ref_cache(ctx);
        }
        Ok(())
    }
}

/// Boundary validation: extract a typed [`MessageId`] from untyped EffectEvent metadata.
///
/// Returns `None` when `message_id` is absent or non-string. Callers that require
/// a message_id for correctness must treat `None` as a validation rejection at the
/// boundary — downstream provenance code must not re-parse this field.
fn message_id_from_metadata(metadata: &Value) -> Option<MessageId> {
    metadata
        .get("message_id")
        .and_then(|value| value.as_str())
        .map(|value| MessageId::from_external(ExternalId::new(value.to_string())))
}

/// Boundary validation: extract a typed [`TaskId`] from untyped EffectEvent metadata.
///
/// Returns `None` when `task_id` is absent or non-string. Callers that require
/// a task_id for correctness must treat `None` as a validation rejection at the
/// boundary — downstream provenance code must not re-parse this field.
fn task_id_from_metadata(metadata: &Value) -> Option<TaskId> {
    metadata
        .get("task_id")
        .and_then(|value| value.as_str())
        .map(|value| TaskId::from_external(ExternalId::new(value.to_string())))
}

fn agent_id_from_metadata(metadata: &Value) -> Option<AgentId> {
    metadata
        .get("agent_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|uuid| AgentId::from_uuid(UuidId::new(uuid)))
}

fn llm_assistant_transcript_message_id(
    metadata: &Value,
    context_id: &ContextId,
    completed_anchor: &ActivityAnchorId,
) -> MessageId {
    message_id_from_metadata(metadata).map_or_else(
        || {
            MessageId::from_external(ExternalId::new(format!(
                "ctx-msg:{}:llm:{}",
                context_id.as_str(),
                completed_anchor.as_str()
            )))
        },
        |base| {
            MessageId::from_external(ExternalId::new(format!(
                "{}:llm:{}",
                base.as_str(),
                completed_anchor.as_str()
            )))
        },
    )
}

async fn emit_llm_assistant_transcript_message<W: ProvenanceWriter + ?Sized>(
    writer: &W,
    context_id: &ContextId,
    metadata: &Value,
    result_payload: Option<&Value>,
    completed_anchor: &ActivityAnchorId,
    timestamp_ms: u64,
    citations: Vec<baml_rt_core::Citation>,
) {
    let Some(task_id) = task_id_from_metadata(metadata) else {
        return;
    };
    let Some(agent_id) = agent_id_from_metadata(metadata) else {
        return;
    };
    let response_text = result_payload
        .map(extract_response_text)
        .unwrap_or_default();
    if response_text.trim().is_empty() {
        return;
    }
    let message_id = llm_assistant_transcript_message_id(metadata, context_id, completed_anchor);
    let msg_event = ProvEvent::message_sent_task(
        context_id.clone(),
        task_id,
        message_id,
        "assistant".to_string(),
        vec![response_text],
        None,
        agent_id,
        timestamp_ms,
        citations,
    );
    writer
        .add_event_with_logging(msg_event, "llm assistant transcript")
        .await;
}

fn usage_tokens(usage: &LlmUsage) -> (Option<u64>, Option<u64>) {
    match usage {
        LlmUsage::Known {
            prompt_tokens,
            completion_tokens,
            ..
        } => (Some(*prompt_tokens), Some(*completion_tokens)),
        LlmUsage::Unknown => (None, None),
    }
}

fn normalized_prompt(prompt: &Value) -> Value {
    if prompt.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        prompt.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use baml_rt_conversation::view::{ProvenanceContextMessage, ProvenanceConversationContextItem};
    use baml_rt_core::{
        Outcome,
        bus::{EffectEvent, LlmEffectMetadata},
    };
    use serde_json::json;

    use super::*;
    use crate::{
        events::ProvEventData,
        store::{ProvenanceContextReader, ProvenanceWriter},
    };

    struct RecordingWriter {
        events: Mutex<Vec<ProvEvent>>,
        seed_messages: Vec<ProvenanceContextMessage>,
    }

    impl Default for RecordingWriter {
        fn default() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                seed_messages: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl ProvenanceContextReader for RecordingWriter {
        async fn context_messages(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceContextMessage>> {
            Ok(self.seed_messages.clone())
        }

        async fn conversation_context(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceConversationContextItem>> {
            Ok(Vec::new())
        }

        async fn conversation_context_with_task(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
            _task_id: Option<&TaskId>,
        ) -> crate::error::Result<Vec<ProvenanceConversationContextItem>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ProvenanceWriter for RecordingWriter {
        async fn add_event(&self, event: ProvEvent) -> crate::error::Result<()> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn tool_started_without_scope_ids_uses_synthetic_message_fallback() {
        let writer = Arc::new(RecordingWriter::default());
        let subscriber = ProvenanceEffectSubscriber::new(writer.clone());
        let context_id = ContextId::from("ctx-gate-synth");
        let event = EffectEvent::ToolStarted {
            context_id: context_id.clone(),
            metadata: baml_rt_core::bus::ToolEffectMetadata {
                tool_name: "support/clickup".to_string(),
                function_name: None,
                args: json!({}),
                metadata: json!({ "agent_id": uuid::Uuid::new_v4().to_string() }),
                delegation_target: None,
                tool_backend: None,
                tool_digest: None,
            },
        };
        subscriber.on_effect(&event).await.unwrap();
        let events = writer.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].data(),
            ProvEventData::ToolCallStarted { .. }
        ));
    }

    #[tokio::test]
    async fn llm_completed_without_citations_has_no_integrity() {
        let writer = Arc::new(RecordingWriter::default());
        let subscriber = ProvenanceEffectSubscriber::new(writer.clone());
        let event = EffectEvent::LlmCompleted {
            context_id: ContextId::from("ctx-1"),
            metadata: LlmEffectMetadata {
                client: "c".into(),
                model: "m".into(),
                function_name: "f".into(),
                prompt: json!({}),
                metadata: json!({"task_id": "task-1", "message_id": "msg-1"}),
                tool_name: baml_rt_core::bus::ToolNameResolution::NotApplicable,
            },
            usage: None,
            result_payload: Some(json!({"message": "hello"})),
            duration_ms: 1,
            outcome: Outcome::Success,
            rejection_reason: None,
        };
        subscriber.on_effect(&event).await.unwrap();
        let events = writer.events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e.data(),
            ProvEventData::LlmCallCompleted {
                citation_integrity: None,
                ..
            }
        )));
    }
}
