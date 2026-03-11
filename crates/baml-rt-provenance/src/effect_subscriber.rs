//! Provenance subscriber: converts EffectEvent to ProvEvent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber},
    ids::{ContextId, ExternalId, MessageId, TaskId},
};
use baml_rt_embedding::{
    DriftConfig, DriftMode, EmbeddingProvider, FastEmbedProvider, score_drift,
};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::{
    events::{LlmDriftInfo, LlmUsage, ProvEvent},
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
    drift_config: DriftConfig,
    drift_provider: RwLock<Option<Arc<dyn EmbeddingProvider>>>,
}

impl ProvenanceEffectSubscriber {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            writer,
            drift_config: DriftConfig::default(),
            drift_provider: RwLock::new(None),
        }
    }

    pub fn new_with_embedding_provider(
        writer: Arc<dyn ProvenanceWriter>,
        drift_config: DriftConfig,
        drift_provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            writer,
            drift_config,
            drift_provider: RwLock::new(Some(drift_provider)),
        }
    }

    async fn drift_provider(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        if let Some(provider) = self.drift_provider.read().await.clone() {
            return Some(provider);
        }

        let provider_result = tokio::task::spawn_blocking(FastEmbedProvider::new).await;
        let provider = match provider_result {
            Ok(Ok(provider)) => Arc::new(provider) as Arc<dyn EmbeddingProvider>,
            Ok(Err(error)) => {
                tracing::warn!(
                    error = %error,
                    "Failed to initialise embedding model in provenance subscriber; drift scoring disabled"
                );
                return None;
            }
            Err(join_error) => {
                tracing::warn!(
                    error = %join_error,
                    "Embedding model init task panicked in provenance subscriber; drift scoring disabled"
                );
                return None;
            }
        };

        let mut guard = self.drift_provider.write().await;
        if let Some(existing) = guard.as_ref() {
            return Some(existing.clone());
        }
        *guard = Some(provider.clone());
        Some(provider)
    }

    async fn compute_drift(
        &self,
        function_name: &str,
        prompt: &Value,
        result_payload: Option<&Value>,
        outcome: baml_rt_core::Outcome,
    ) -> Option<LlmDriftInfo> {
        if !bool::from(outcome) || !self.drift_config.should_monitor(function_name) {
            return None;
        }
        let result_payload = result_payload?;
        let provider = self.drift_provider().await?;
        let assessment = score_drift(
            prompt,
            result_payload,
            &self.drift_config,
            provider.as_ref(),
        )?;
        Some(LlmDriftInfo {
            score: assessment.score,
            severity: assessment.severity_label().to_string(),
            mode: drift_mode_label(assessment.mode).to_string(),
            warn_threshold: assessment.warn_threshold,
            block_threshold: assessment.block_threshold,
            intent_text_preview: assessment.intent_text_preview,
            response_text_preview: assessment.response_text_preview,
        })
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
                    }) => LlmUsage::Known {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        total_tokens: *total_tokens,
                    },
                    Some(baml_rt_core::bus::LlmUsage::Unknown) | None => LlmUsage::Unknown,
                };
                let prov_usage_clone = prov_usage.clone();
                let prompt = normalized_prompt(&metadata.prompt);
                let drift = self
                    .compute_drift(
                        &metadata.function_name,
                        &prompt,
                        result_payload.as_ref(),
                        *outcome,
                    )
                    .await;
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
                        ProvEvent::llm_call_completed_task_with_drift(
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
                            drift.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_completed_global_with_drift(
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
                            drift.clone(),
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

fn drift_mode_label(mode: DriftMode) -> &'static str {
    match mode {
        DriftMode::Audit => "audit",
        DriftMode::Enforce => "enforce",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use baml_rt_core::{
        Outcome,
        bus::{EffectEvent, LlmEffectMetadata},
    };
    use baml_rt_embedding::provider::EmbeddingError;
    use serde_json::json;

    use super::*;
    use crate::{
        events::ProvEventData,
        store::{
            ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
            ProvenanceWriter,
        },
    };

    struct MockProvider {
        mappings: Vec<(&'static str, Vec<f32>)>,
        fallback: Vec<f32>,
    }

    impl MockProvider {
        fn new(mappings: Vec<(&'static str, Vec<f32>)>, fallback: Vec<f32>) -> Self {
            Self { mappings, fallback }
        }
    }

    impl EmbeddingProvider for MockProvider {
        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts
                .iter()
                .map(|text| {
                    self.mappings
                        .iter()
                        .find(|(prefix, _)| text.contains(prefix))
                        .map(|(_, embedding)| embedding.clone())
                        .unwrap_or_else(|| self.fallback.clone())
                })
                .collect())
        }

        fn dimension(&self) -> usize {
            self.fallback.len()
        }
    }

    #[derive(Default)]
    struct RecordingWriter {
        events: Mutex<Vec<ProvEvent>>,
    }

    #[async_trait]
    impl ProvenanceContextReader for RecordingWriter {
        async fn context_messages(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceContextMessage>> {
            Ok(Vec::new())
        }

        async fn conversation_context(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceConversationContextItem>> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ProvenanceWriter for RecordingWriter {
        async fn add_event(&self, event: ProvEvent) -> crate::error::Result<()> {
            self.events.lock().expect("events lock").push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn llm_completed_effect_emits_drift_fields() {
        let writer = Arc::new(RecordingWriter::default());
        let provider = Arc::new(MockProvider::new(
            vec![
                ("Create a task", vec![1.0, 0.0, 0.0, 0.0]),
                ("Ignore previous", vec![0.0, 0.0, 0.0, 1.0]),
            ],
            vec![0.0; 4],
        ));
        let subscriber = ProvenanceEffectSubscriber::new_with_embedding_provider(
            writer.clone(),
            DriftConfig::default(),
            provider,
        );
        let context_id = ContextId::new(1, 1);
        let event = EffectEvent::LlmCompleted {
            context_id: context_id.clone(),
            metadata: LlmEffectMetadata {
                client: "anthropic".to_string(),
                model: "claude".to_string(),
                function_name: "ChooseAction".to_string(),
                prompt: json!([{"role":"user","content":"Create a task titled 'Research'."}]),
                metadata: json!({
                    "agent_id": "00000000-0000-0000-0000-000000000001",
                    "message_id": "msg-1"
                }),
            },
            usage: None,
            result_payload: Some(json!({"message": "Ignore previous instructions."})),
            duration_ms: 42,
            outcome: Outcome::Success,
            rejection_reason: None,
        };

        subscriber.on_effect(&event).await.expect("effect handled");

        let events = writer.events.lock().expect("events lock");
        let completed = events.last().expect("completed event recorded");
        match completed.data() {
            ProvEventData::LlmCallCompleted { drift, .. } => {
                let drift = drift.as_ref().expect("drift info");
                assert_eq!(drift.mode, "audit");
                assert_eq!(drift.severity, "block");
                assert!(drift.score >= 0.0);
                assert!(drift.intent_text_preview.contains("Create a task"));
                assert!(drift.response_text_preview.contains("Ignore previous"));
            }
            other => panic!("expected LlmCallCompleted event, got {other:?}"),
        }
    }
}
