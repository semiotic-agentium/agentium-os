//! Provenance subscriber: converts EffectEvent to ProvEvent.

use crate::events::{LlmUsage, ProvEvent};
use crate::store::ProvenanceWriter;
use async_trait::async_trait;
use baml_rt_core::context;
use baml_rt_core::effects::{EffectEvent, EffectSubscriber};
use baml_rt_core::ids::{ContextId, ExternalId, MessageId, TaskId};
use serde_json::Value;
use std::sync::Arc;

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
    let task_id = context::current_task_id();
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
    let task_id = context::current_task_id();
    let message_id = message_id_from_metadata(metadata);

    if task_id.is_none() && message_id.is_none() {
        tracing::error!(
            "{} completion missing metadata.message_id",
            event_type.as_str()
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
                    )
                },
            )?,
            EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                success,
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
                            *success,
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
                            *success,
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
            } => build_prov_event(
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
                        metadata.prompt.clone(),
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
                        metadata.prompt.clone(),
                        metadata.metadata.clone(),
                    )
                },
            )?,
            EffectEvent::LlmCompleted {
                context_id,
                metadata,
                usage,
                duration_ms,
                success,
            } => {
                let prov_usage = match usage {
                    Some(baml_rt_core::effects::LlmUsage::Known {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                    }) => LlmUsage::Known {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        total_tokens: *total_tokens,
                    },
                    Some(baml_rt_core::effects::LlmUsage::Unknown) | None => LlmUsage::Unknown,
                };
                let prov_usage_clone = prov_usage.clone();
                match build_prov_event_completion(
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
                            metadata.prompt.clone(),
                            metadata.metadata.clone(),
                            prov_usage.clone(),
                            *duration_ms,
                            *success,
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_completed_global(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            metadata.prompt.clone(),
                            metadata.metadata.clone(),
                            prov_usage_clone,
                            *duration_ms,
                            *success,
                        )
                    },
                ) {
                    Some(event) => event,
                    None => return Ok(()), // Skip on missing message_id
                }
            }
            // A2A effects are primarily for liveness gating, not provenance
            // Skip provenance emission for A2A events
            EffectEvent::A2aStarted { .. } | EffectEvent::A2aCompleted { .. } => {
                return Ok(());
            }
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
