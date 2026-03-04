//! Provenance subscriber: converts EffectEvent to ProvEvent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber},
    ids::{ContextId, ExternalId, MessageId, TaskId},
};
use serde_json::Value;

use crate::{
    events::{LlmUsage, ProvEvent},
    store::ProvenanceWriter,
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

/// Helper to build provenance events with task/global branching
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
    let task_id = task_id_from_metadata(metadata);
    let message_id = message_id_from_metadata(metadata);

    if task_id.is_none() && message_id.is_none() {
        return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
            "{} missing metadata.message_id",
            event_type.as_str()
        )));
    }

    Ok(if let Some(task_id) = task_id {
        build_task(context_id.clone(), task_id)
    } else {
        let message_id = message_id.ok_or_else(|| {
            baml_rt_core::BamlRtError::InvalidArgument(format!(
                "{} missing metadata.message_id",
                event_type.as_str()
            ))
        })?;
        build_global(context_id.clone(), message_id)
    })
}

/// Helper for completion events that may skip on missing message_id
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
    let task_id = task_id_from_metadata(metadata);
    let message_id = message_id_from_metadata(metadata);

    if task_id.is_none() && message_id.is_none() {
        tracing::error!(
            event_type = event_type.as_str(),
            "completion missing metadata.message_id"
        );
        return None;
    }

    Some(if let Some(task_id) = task_id {
        build_task(context_id.clone(), task_id)
    } else {
        let message_id = message_id?;
        build_global(context_id.clone(), message_id)
    })
}

/// Adapter that subscribes to effect events and emits provenance events.
pub struct ProvenanceEffectSubscriber {
    writer: Arc<dyn ProvenanceWriter>,
}

impl ProvenanceEffectSubscriber {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl EffectSubscriber for ProvenanceEffectSubscriber {
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
                },
            )?,
            EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                outcome,
            } => {
                match build_prov_event_completion(
                    context_id,
                    &metadata.metadata,
                    ProvenanceEventType::ToolCall,
                    |ctx_id, task_id| {
                        ProvEvent::tool_call_completed_task(
                            ctx_id,
                            task_id,
                            metadata.tool_name.clone(),
                            metadata.function_name.clone(),
                            metadata.args.clone(),
                            metadata.metadata.clone(),
                            *duration_ms,
                            *outcome,
                            metadata.delegation_target.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::tool_call_completed_global(
                            ctx_id,
                            msg_id,
                            metadata.tool_name.clone(),
                            metadata.function_name.clone(),
                            metadata.args.clone(),
                            metadata.metadata.clone(),
                            *duration_ms,
                            *outcome,
                            metadata.delegation_target.clone(),
                        )
                    },
                ) {
                    Some(event) => event,
                    None => return Ok(()), // Skip on missing message_id
                }
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
                duration_ms,
                outcome,
                rejection_reason,
            } => {
                let prov_usage = match usage {
                    Some(baml_rt_core::bus::LlmUsage::Known {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                    }) => LlmUsage::Known {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        total_tokens: *total_tokens,
                    },
                    Some(baml_rt_core::bus::LlmUsage::Unknown) | None => LlmUsage::Unknown,
                };
                let prov_usage_clone = prov_usage.clone();
                let prompt = normalized_prompt(&metadata.prompt);
                let Some(completed_event) = build_prov_event_completion(
                    context_id,
                    &metadata.metadata,
                    ProvenanceEventType::LlmCall,
                    |ctx_id, task_id| {
                        ProvEvent::llm_call_completed_task(
                            ctx_id,
                            task_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                            prov_usage.clone(),
                            *duration_ms,
                            *outcome,
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_completed_global(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                            prov_usage_clone,
                            *duration_ms,
                            *outcome,
                        )
                    },
                ) else {
                    return Ok(()); // Skip on missing message_id
                };
                let completed_id = completed_event.id().clone();
                self.writer
                    .add_event_with_logging(completed_event, "effect subscriber")
                    .await;
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
                }
                return Ok(());
            }
            // A2A effects are primarily for liveness gating, not provenance
            // Skip provenance emission for A2A events
            EffectEvent::A2aStarted { .. } | EffectEvent::A2aCompleted { .. } => {
                return Ok(());
            }
            // Tool stream chunks are relay-only; tools are already recorded via the tool interceptor
            EffectEvent::ToolStreamChunk { .. } => return Ok(()),
        };

        self.writer
            .add_event_with_logging(prov_event, "effect subscriber")
            .await;
        Ok(())
    }
}

fn message_id_from_metadata(metadata: &Value) -> Option<MessageId> {
    metadata
        .get("message_id")
        .and_then(|value| value.as_str())
        .map(|value| MessageId::from_external(ExternalId::new(value.to_string())))
}

fn task_id_from_metadata(metadata: &Value) -> Option<TaskId> {
    metadata
        .get("task_id")
        .and_then(|value| value.as_str())
        .map(|value| TaskId::from_external(ExternalId::new(value.to_string())))
}

fn normalized_prompt(prompt: &Value) -> Value {
    if prompt.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        prompt.clone()
    }
}
