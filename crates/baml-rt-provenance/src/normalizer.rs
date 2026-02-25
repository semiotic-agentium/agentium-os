use std::collections::HashMap;

use baml_rt_core::ids::{
    AgentId, ArtifactId, ContextId, EventId, ExternalId, MessageId, ProvVocabularyType, TaskId,
    UuidId,
};
use serde_json::Value;

use crate::{
    document::ProvDocument,
    error::{ProvenanceError, Result},
    events::{CallScope, ProvEvent, ProvEventData},
    id_semantics::{
        AgentBootActivityId, AgentBootActivityInput, AgentRuntimeInstanceId,
        AgentRuntimeInstanceInput, ArchiveEntityId, ArchiveEntityInput, ArtifactByEventEntityId,
        ArtifactByEventEntityInput, ArtifactByIdEntityId, ArtifactByIdEntityInput,
        ArtifactByTypeEntityId, ArtifactByTypeEntityInput, ArtifactIdentity, LlmCallActivityId,
        LlmCallActivityInput, LlmPromptEntityId, LlmPromptEntityInput, MessageEntityId,
        MessageEntityInput, MessageProcessingActivityId, MessageProcessingActivityInput,
        RunnerRuntimeInstanceId, TaskEntityId, TaskEntityInput, TaskExecutionActivityId,
        TaskExecutionActivityInput, TaskStateEntityId, TaskStateEntityInput, TaskStatePrevEntityId,
        TaskStatePrevEntityInput, ToolArgsEntityId, ToolArgsEntityInput, ToolCallActivityId,
        ToolCallActivityInput,
    },
    types::{
        Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef,
        QualifiedGeneration, Used, WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
    },
    vocabulary::{
        a2a, a2a_relation_types, a2a_relations, a2a_roles, agent_types, message_directions,
        prov_roles,
    },
};

#[derive(Debug, Clone)]
pub struct NormalizedProv {
    pub document: ProvDocument,
    pub derived_relations: Vec<A2aDerivedRelation>,
    pub agent_labels: HashMap<String, String>,
}

fn parse_agent_id(event: &ProvEvent, raw: &str) -> Result<AgentId> {
    UuidId::parse_str(raw)
        .map(AgentId::from_uuid)
        .map_err(|_| ProvenanceError::InvalidEvent {
            event_id: event.id().as_str().to_string(),
            reason: format!("invalid agent_id '{}' (expected UUID)", raw),
        })
}

pub trait ProvNormalizer: Send + Sync {
    fn normalize(&self, event: &ProvEvent) -> Result<NormalizedProv>;
}

#[derive(Debug, Default)]
pub struct DefaultProvNormalizer {
    agent_registry: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl ProvNormalizer for DefaultProvNormalizer {
    fn normalize(&self, event: &ProvEvent) -> Result<NormalizedProv> {
        let mut registry = self.agent_registry.lock().expect("agent registry lock");
        normalize_event_with_registry(event, &mut registry)
    }
}

#[derive(Debug, Clone)]
pub struct A2aDerivedRelation {
    pub relation: A2aRelationType,
    pub from: ProvNodeRef,
    pub to: ProvNodeRef,
    pub attributes: HashMap<String, Value>,
}

#[derive(Debug, Clone, Copy)]
pub enum A2aRelationType {
    TaskHasMessage,
    TaskHasArtifact,
    TaskCall,
    TaskStatusTransition,
    MessageCall,
}

impl A2aRelationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            A2aRelationType::TaskHasMessage => a2a_relations::TASK_MESSAGE,
            A2aRelationType::TaskHasArtifact => a2a_relations::TASK_ARTIFACT,
            A2aRelationType::TaskCall => a2a_relations::TASK_CALL,
            A2aRelationType::TaskStatusTransition => a2a_relations::TASK_STATUS_TRANSITION,
            A2aRelationType::MessageCall => a2a_relations::MESSAGE_CALL,
        }
    }
}

fn prov_type<S: ProvVocabularyType>() -> String {
    S::VOCAB_TYPE.to_string()
}

pub fn normalize_event(event: &ProvEvent) -> Result<NormalizedProv> {
    normalize_event_with_registry(event, &mut std::collections::HashSet::new())
}

fn normalize_event_with_registry(
    event: &ProvEvent,
    agent_registry: &mut std::collections::HashSet<String>,
) -> Result<NormalizedProv> {
    let mut doc = ProvDocument::new();
    let mut derived_relations = Vec::new();
    let mut agent_labels = HashMap::new();

    match event.data() {
        ProvEventData::LlmCallStarted {
            scope,
            client,
            model,
            function_name,
            prompt,
            metadata,
        } => {
            let activity_id = llm_activity_id(event.id());
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::CLIENT.to_string(), Value::String(client.clone()));
            attrs.insert(a2a::MODEL.to_string(), Value::String(model.clone()));
            attrs.insert(
                a2a::FUNCTION_NAME.to_string(),
                Value::String(function_name.clone()),
            );
            attrs.insert(a2a::METADATA.to_string(), metadata.clone());
            let start_time_ms = Some(event.timestamp_ms());

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms,
                    end_time_ms: None,
                    prov_type: Some(prov_type::<LlmCallActivityId>()),
                    attributes: attrs,
                },
            );

            let prompt_id = llm_prompt_entity_id(event.id());
            let mut prompt_attrs = base_attrs(event);
            prompt_attrs.insert(a2a::PROMPT.to_string(), prompt.clone());
            doc.insert_entity(
                prompt_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<LlmPromptEntityId>()),
                    attributes: prompt_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                prompt_id,
                Some(a2a_roles::PROMPT.to_string()),
            );
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                agent_registry,
                &mut agent_labels,
            )?;
        }
        ProvEventData::LlmCallCompleted {
            scope,
            client,
            model,
            function_name,
            prompt,
            metadata,
            usage,
            duration_ms,
            outcome,
        } => {
            let activity_id = llm_activity_id(event.id());
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::CLIENT.to_string(), Value::String(client.clone()));
            attrs.insert(a2a::MODEL.to_string(), Value::String(model.clone()));
            attrs.insert(
                a2a::FUNCTION_NAME.to_string(),
                Value::String(function_name.clone()),
            );
            attrs.insert(a2a::METADATA.to_string(), metadata.clone());
            match usage {
                crate::events::LlmUsage::Known {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                } => {
                    attrs.insert(
                        a2a::USAGE_PROMPT_TOKENS.to_string(),
                        Value::Number((*prompt_tokens).into()),
                    );
                    attrs.insert(
                        a2a::USAGE_COMPLETION_TOKENS.to_string(),
                        Value::Number((*completion_tokens).into()),
                    );
                    attrs.insert(
                        a2a::USAGE_TOTAL_TOKENS.to_string(),
                        Value::Number((*total_tokens).into()),
                    );
                }
                crate::events::LlmUsage::Unknown => {}
            }
            attrs.insert(
                a2a::DURATION_MS.to_string(),
                Value::Number((*duration_ms).into()),
            );
            attrs.insert(a2a::SUCCESS.to_string(), Value::Bool(bool::from(*outcome)));

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms: None,
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<LlmCallActivityId>()),
                    attributes: attrs,
                },
            );

            let prompt_id = llm_prompt_entity_id(event.id());
            let mut prompt_attrs = base_attrs(event);
            prompt_attrs.insert(a2a::PROMPT.to_string(), prompt.clone());
            doc.insert_entity(
                prompt_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<LlmPromptEntityId>()),
                    attributes: prompt_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                prompt_id,
                Some(a2a_roles::PROMPT.to_string()),
            );
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                agent_registry,
                &mut agent_labels,
            )?;
        }
        ProvEventData::ToolCallStarted {
            scope,
            tool_name,
            function_name,
            args,
            metadata,
        } => {
            let activity_id = tool_activity_id(event.id());
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::TOOL_NAME.to_string(), Value::String(tool_name.clone()));
            if let Some(function_name) = function_name {
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(function_name.clone()),
                );
            }
            attrs.insert(a2a::METADATA.to_string(), metadata.clone());
            let start_time_ms = Some(event.timestamp_ms());

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms,
                    end_time_ms: None,
                    prov_type: Some(prov_type::<ToolCallActivityId>()),
                    attributes: attrs,
                },
            );

            let args_id = tool_args_entity_id(event.id());
            let mut args_attrs = base_attrs(event);
            args_attrs.insert(a2a::ARGS.to_string(), args.clone());
            doc.insert_entity(
                args_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ToolArgsEntityId>()),
                    attributes: args_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                args_id,
                Some(a2a_roles::ARGS.to_string()),
            );
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                agent_registry,
                &mut agent_labels,
            )?;
        }
        ProvEventData::ToolCallCompleted {
            scope,
            tool_name,
            function_name,
            args,
            metadata,
            duration_ms,
            outcome,
        } => {
            let activity_id = tool_activity_id(event.id());
            let mut attrs = base_attrs(event);
            attrs.insert(a2a::TOOL_NAME.to_string(), Value::String(tool_name.clone()));
            if let Some(function_name) = function_name {
                attrs.insert(
                    a2a::FUNCTION_NAME.to_string(),
                    Value::String(function_name.clone()),
                );
            }
            attrs.insert(a2a::METADATA.to_string(), metadata.clone());
            attrs.insert(
                a2a::DURATION_MS.to_string(),
                Value::Number((*duration_ms).into()),
            );
            attrs.insert(a2a::SUCCESS.to_string(), Value::Bool(bool::from(*outcome)));

            doc.insert_activity(
                activity_id.clone(),
                Activity {
                    start_time_ms: None,
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<ToolCallActivityId>()),
                    attributes: attrs,
                },
            );

            let args_id = tool_args_entity_id(event.id());
            let mut args_attrs = base_attrs(event);
            args_attrs.insert(a2a::ARGS.to_string(), args.clone());
            doc.insert_entity(
                args_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ToolArgsEntityId>()),
                    attributes: args_attrs,
                },
            );
            insert_used(
                &mut doc,
                activity_id.clone(),
                args_id,
                Some(a2a_roles::ARGS.to_string()),
            );
            if let Some(message_id) = scoped_message_id(scope, metadata) {
                attach_message_context(
                    &mut doc,
                    event,
                    &activity_id,
                    &message_id,
                    &mut derived_relations,
                );
            }
            attach_task_call_context(
                &mut doc,
                event,
                &activity_id,
                &mut derived_relations,
                agent_registry,
                &mut agent_labels,
            )?;
        }
        ProvEventData::AgentBooted {
            agent_id,
            agent_type,
            agent_version,
            archive_path,
        } => {
            agent_registry.insert(agent_id.as_str().to_string());
            // Create AgentArchive entity
            let archive_entity_id = archive_entity_id(archive_path);
            let mut archive_attrs = base_attrs(event);
            archive_attrs.insert(
                a2a::ARCHIVE_PATH.to_string(),
                Value::String(archive_path.clone()),
            );
            doc.insert_entity(
                archive_entity_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<ArchiveEntityId>()),
                    attributes: archive_attrs,
                },
            );

            // Create AgentBoot activity
            let boot_activity_id = boot_activity_id(agent_id);
            let mut boot_attrs = base_attrs(event);
            boot_attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(agent_id.as_str().to_string()),
            );
            boot_attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent_type.as_str().to_string()),
            );
            boot_attrs.insert(
                a2a::AGENT_VERSION.to_string(),
                Value::String(agent_version.clone()),
            );
            doc.insert_activity(
                boot_activity_id.clone(),
                Activity {
                    start_time_ms: Some(event.timestamp_ms()),
                    end_time_ms: Some(event.timestamp_ms()),
                    prov_type: Some(prov_type::<AgentBootActivityId>()),
                    attributes: boot_attrs,
                },
            );

            // Link archive --USED--> boot
            insert_used(
                &mut doc,
                boot_activity_id.clone(),
                archive_entity_id,
                Some(a2a_roles::ARCHIVE.to_string()),
            );

            // Create AgentRuntimeInstance agent
            let instance_agent_id = agent_runtime_instance_id(agent_id);
            let mut instance_attrs = base_attrs(event);
            instance_attrs.insert(
                a2a::AGENT_ID.to_string(),
                Value::String(agent_id.as_str().to_string()),
            );
            instance_attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent_type.as_str().to_string()),
            );
            instance_attrs.insert(
                a2a::AGENT_VERSION.to_string(),
                Value::String(agent_version.clone()),
            );
            doc.insert_agent(
                instance_agent_id.clone(),
                Agent {
                    prov_type: Some(prov_type::<AgentRuntimeInstanceId>()),
                    attributes: instance_attrs,
                },
            );

            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Agent(instance_agent_id.clone()),
                boot_activity_id.clone(),
                Some(event.timestamp_ms()),
            );
            insert_qualified_generation(
                &mut doc,
                ProvNodeRef::Agent(instance_agent_id.clone()),
                boot_activity_id.clone(),
                Some(event.timestamp_ms()),
            );

            // Link boot activity to runner runtime instance via association role.
            let runner_runtime_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                boot_activity_id,
                runner_runtime_id,
                Some(prov_roles::EXECUTING_AGENT.to_string()),
            );
        }
        ProvEventData::TaskCreated { task_id, agent_id } => {
            let task_entity = ensure_task_entity(&mut doc, task_id, event.context_id(), None);

            // Store agent_id in task entity for later lookups
            let task_entity_id = task_entity_id(task_id);
            if let Some(entity) = doc.entity(&task_entity_id) {
                let mut attrs = entity.attributes.clone();
                attrs.insert(
                    a2a::AGENT_ID.to_string(),
                    Value::String(agent_id.as_str().to_string()),
                );
                doc.insert_entity(
                    task_entity_id.clone(),
                    Entity {
                        prov_type: entity.prov_type.clone(),
                        attributes: attrs,
                    },
                );
            }

            // Look up agent_type from runtime instance agent - AgentBooted must have been written first
            let agent_instance_id = agent_runtime_instance_id(agent_id);
            let agent_type = doc
                .agent(&agent_instance_id)
                .and_then(|agent| agent.attributes.get(a2a::AGENT_TYPE))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                event.context_id(),
                Some(event.timestamp_ms()),
                None,
                agent_type.as_deref(),
                agent_registry,
                &mut agent_labels,
            )?;
            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Entity(task_entity.clone()),
                task_execution.clone(),
                Some(event.timestamp_ms()),
            );

            let agent_instance_id =
                get_agent_runtime_instance(&doc, agent_id, agent_registry, &mut agent_labels)?;
            insert_was_associated_with(
                &mut doc,
                task_execution.clone(),
                agent_instance_id,
                Some(prov_roles::EXECUTING_AGENT.to_string()),
            );

            let invoking_agent_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                task_execution,
                invoking_agent_id,
                Some(prov_roles::INVOKING_AGENT.to_string()),
            );
        }
        ProvEventData::TaskStatusChanged {
            task_id,
            old_status,
            new_status,
        } => {
            let _task_entity = ensure_task_entity(&mut doc, task_id, event.context_id(), None);
            let is_terminal = new_status
                .as_deref()
                .map(is_terminal_status)
                .unwrap_or(false);
            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                event.context_id(),
                None,
                is_terminal.then_some(event.timestamp_ms()),
                None,
                agent_registry,
                &mut agent_labels,
            )?;
            let status_id = task_state_entity_id(task_id, event.timestamp_ms());
            let mut status_attrs = base_attrs(event);
            status_attrs.insert(
                a2a::TASK_STATE_TIME.to_string(),
                Value::Number(event.timestamp_ms().into()),
            );
            if let Some(new_status) = new_status {
                status_attrs.insert(
                    a2a::TASK_STATE.to_string(),
                    Value::String(new_status.clone()),
                );
            }
            if let Some(old_status) = old_status {
                status_attrs.insert(
                    a2a::OLD_STATUS.to_string(),
                    Value::String(old_status.clone()),
                );
            }
            doc.insert_entity(
                status_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<TaskStateEntityId>()),
                    attributes: status_attrs,
                },
            );
            insert_used(
                &mut doc,
                task_execution.clone(),
                status_id.clone(),
                Some(a2a_roles::TASK_STATE.to_string()),
            );

            if is_terminal {
                let task_entity = task_entity_id(task_id);
                insert_was_generated_by(
                    &mut doc,
                    ProvNodeRef::Entity(task_entity),
                    task_execution.clone(),
                    Some(event.timestamp_ms()),
                );
            }

            if let Some(old_status) = old_status {
                let old_id = task_state_prev_entity_id(task_id, event.timestamp_ms());
                let mut old_attrs = base_attrs(event);
                old_attrs.insert(
                    a2a::TASK_STATE_TIME.to_string(),
                    Value::Number(event.timestamp_ms().into()),
                );
                old_attrs.insert(
                    a2a::TASK_STATE.to_string(),
                    Value::String(old_status.clone()),
                );
                old_attrs.insert(a2a::IS_PREVIOUS.to_string(), Value::Bool(true));
                doc.insert_entity(
                    old_id.clone(),
                    Entity {
                        prov_type: Some(prov_type::<TaskStatePrevEntityId>()),
                        attributes: old_attrs,
                    },
                );
                insert_was_derived_from(
                    &mut doc,
                    status_id.clone(),
                    old_id.clone(),
                    Some(task_execution.clone()),
                    Some(a2a_relation_types::STATUS_TRANSITION.to_string()),
                );
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::TaskStatusTransition,
                    from: ProvNodeRef::Entity(old_id),
                    to: ProvNodeRef::Entity(status_id),
                    attributes: derived_attrs(event),
                });
            }
        }
        ProvEventData::TaskArtifactGenerated {
            task_id,
            artifact_id,
            artifact_type,
        } => {
            let task_entity = ensure_task_entity(&mut doc, task_id, event.context_id(), None);
            let task_execution = ensure_task_execution_activity(
                &mut doc,
                task_id,
                event.context_id(),
                None,
                None,
                None,
                agent_registry,
                &mut agent_labels,
            )?;
            let artifact_id_str =
                artifact_entity_id(task_id, artifact_id, artifact_type, event.id());
            let mut artifact_attrs = base_attrs(event);
            if let Some(artifact_id) = artifact_id {
                artifact_attrs.insert(
                    a2a::ARTIFACT_ID.to_string(),
                    Value::String(artifact_id.as_str().to_string()),
                );
            }
            if let Some(artifact_type) = artifact_type {
                artifact_attrs.insert(
                    a2a::ARTIFACT_TYPE.to_string(),
                    Value::String(artifact_type.clone()),
                );
            }
            doc.insert_entity(
                artifact_id_str.clone(),
                Entity {
                    prov_type: Some(if artifact_id.is_some() {
                        prov_type::<ArtifactByIdEntityId>()
                    } else if artifact_type.is_some() {
                        prov_type::<ArtifactByTypeEntityId>()
                    } else {
                        prov_type::<ArtifactByEventEntityId>()
                    }),
                    attributes: artifact_attrs,
                },
            );
            insert_was_generated_by(
                &mut doc,
                ProvNodeRef::Entity(artifact_id_str.clone()),
                task_execution.clone(),
                Some(event.timestamp_ms()),
            );
            derived_relations.push(A2aDerivedRelation {
                relation: A2aRelationType::TaskHasArtifact,
                from: ProvNodeRef::Entity(task_entity),
                to: ProvNodeRef::Entity(artifact_id_str),
                attributes: derived_attrs(event),
            });
        }
        ProvEventData::MessageReceived {
            id,
            role,
            content,
            metadata,
        }
        | ProvEventData::MessageSent {
            id,
            role,
            content,
            metadata,
        } => {
            let message_id = message_entity_id(event.context_id(), id);
            let mut message_attrs = base_attrs(event);
            message_attrs.insert(
                a2a::MESSAGE_ID.to_string(),
                Value::String(id.as_str().to_string()),
            );
            message_attrs.insert(a2a::ROLE.to_string(), Value::String(role.clone()));
            let content_values: Vec<Value> = content
                .iter()
                .map(|line| Value::String(line.clone()))
                .collect();
            message_attrs.insert(a2a::CONTENT.to_string(), Value::Array(content_values));
            if let Some(metadata) = metadata {
                message_attrs.insert(a2a::METADATA.to_string(), map_string_map(metadata));
            }

            let direction = if matches!(event.data(), ProvEventData::MessageReceived { .. }) {
                message_directions::RECEIVED
            } else {
                message_directions::SENT
            };
            message_attrs.insert(
                a2a::DIRECTION.to_string(),
                Value::String(direction.to_string()),
            );

            doc.insert_entity(
                message_id.clone(),
                Entity {
                    prov_type: Some(prov_type::<MessageEntityId>()),
                    attributes: message_attrs,
                },
            );

            let processing_id = message_processing_activity_id(event.context_id(), id);
            let mut processing_attrs = base_attrs(event);
            processing_attrs.insert(
                a2a::MESSAGE_ID.to_string(),
                Value::String(id.as_str().to_string()),
            );
            processing_attrs.insert(
                a2a::DIRECTION.to_string(),
                Value::String(direction.to_string()),
            );
            processing_attrs.insert(a2a::ROLE.to_string(), Value::String(role.clone()));
            doc.insert_activity(
                processing_id.clone(),
                Activity {
                    start_time_ms: Some(event.timestamp_ms()),
                    end_time_ms: None,
                    prov_type: Some(prov_type::<MessageProcessingActivityId>()),
                    attributes: processing_attrs,
                },
            );

            // metadata is optional for A2A messages; only associate executing agent when present.
            let agent_id = metadata
                .as_ref()
                .and_then(|m| m.get("agent_id"))
                .map(|raw| parse_agent_id(event, raw))
                .transpose()?;
            if let Some(agent_id) = agent_id.as_ref() {
                let executing_agent_id =
                    get_agent_runtime_instance(&doc, agent_id, agent_registry, &mut agent_labels)?;
                insert_was_associated_with(
                    &mut doc,
                    processing_id.clone(),
                    executing_agent_id,
                    Some(prov_roles::EXECUTING_AGENT.to_string()),
                );
            }

            let invoking_agent_id = runner_runtime_instance_id();
            ensure_runner_runtime_instance(&mut doc);
            insert_was_associated_with(
                &mut doc,
                processing_id.clone(),
                invoking_agent_id,
                Some(prov_roles::INVOKING_AGENT.to_string()),
            );

            match event.data() {
                ProvEventData::MessageReceived { .. } => {
                    insert_used(
                        &mut doc,
                        processing_id.clone(),
                        message_id.clone(),
                        Some(a2a_roles::INPUT_MESSAGE.to_string()),
                    );
                }
                ProvEventData::MessageSent { .. } => {
                    insert_was_generated_by(
                        &mut doc,
                        ProvNodeRef::Entity(message_id.clone()),
                        processing_id.clone(),
                        Some(event.timestamp_ms()),
                    );
                }
                _ => {}
            }

            if let Some(task_id) = event.task_id() {
                // Ensure task entity exists and has agent_id set from message metadata
                let task_entity = ensure_task_entity(&mut doc, task_id, event.context_id(), None);
                let task_entity_id = task_entity_id(task_id);
                if let Some(entity) = doc.entity(&task_entity_id) {
                    let mut attrs = entity.attributes.clone();
                    // Set agent_id on task entity when available from message metadata.
                    if !attrs.contains_key(a2a::AGENT_ID)
                        && let Some(agent_id_ref) = agent_id.as_ref()
                    {
                        attrs.insert(
                            a2a::AGENT_ID.to_string(),
                            Value::String(agent_id_ref.as_str().to_string()),
                        );
                        doc.insert_entity(
                            task_entity_id.clone(),
                            Entity {
                                prov_type: entity.prov_type.clone(),
                                attributes: attrs,
                            },
                        );
                    }
                }

                let task_execution = ensure_task_execution_activity(
                    &mut doc,
                    task_id,
                    event.context_id(),
                    None,
                    None,
                    None,
                    agent_registry,
                    &mut agent_labels,
                )?;
                if matches!(event.data(), ProvEventData::MessageReceived { .. }) {
                    insert_used(
                        &mut doc,
                        task_execution.clone(),
                        message_id.clone(),
                        Some(a2a_roles::INPUT_MESSAGE.to_string()),
                    );
                }
                let mut attrs = derived_attrs(event);
                attrs.insert(
                    a2a::DIRECTION.to_string(),
                    Value::String(direction.to_string()),
                );
                derived_relations.push(A2aDerivedRelation {
                    relation: A2aRelationType::TaskHasMessage,
                    from: ProvNodeRef::Entity(task_entity),
                    to: ProvNodeRef::Entity(message_id),
                    attributes: attrs,
                });
            }
        }
    }

    Ok(NormalizedProv {
        document: doc,
        derived_relations,
        agent_labels,
    })
}

pub fn validate_event(event: &ProvEvent) -> Result<()> {
    match event.data() {
        ProvEventData::LlmCallStarted {
            scope, metadata, ..
        }
        | ProvEventData::LlmCallCompleted {
            scope, metadata, ..
        } => {
            validate_call_scope(event, scope, "llm call")?;
            validate_required_call_metadata(event, scope, metadata, "llm call")?;
        }
        ProvEventData::ToolCallStarted {
            scope, metadata, ..
        }
        | ProvEventData::ToolCallCompleted {
            scope, metadata, ..
        } => {
            validate_call_scope(event, scope, "tool call")?;
            validate_required_call_metadata(event, scope, metadata, "tool call")?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_call_scope(event: &ProvEvent, scope: &CallScope, call_kind: &str) -> Result<()> {
    let event_id = event.id().as_str().to_string();
    match (event, scope) {
        (ProvEvent::Global(_), CallScope::Message { .. }) => Ok(()),
        (ProvEvent::Global(_), CallScope::Task { .. }) => Err(ProvenanceError::InvalidEvent {
            event_id: event_id.clone(),
            reason: format!("{call_kind} is task-scoped but event is global"),
        }),
        (ProvEvent::Task(_event), CallScope::Message { .. }) => {
            Err(ProvenanceError::InvalidEvent {
                event_id: event_id.clone(),
                reason: format!("{call_kind} is message-scoped but event is task-scoped"),
            })
        }
        (ProvEvent::Task(event), CallScope::Task { task_id }) => {
            if task_id == &event.task_id {
                Ok(())
            } else {
                Err(ProvenanceError::InvalidEvent {
                    event_id,
                    reason: format!("{call_kind} task_id does not match event task_id"),
                })
            }
        }
    }
}

fn validate_required_call_metadata(
    event: &ProvEvent,
    scope: &CallScope,
    metadata: &Value,
    call_kind: &str,
) -> Result<()> {
    let event_id = event.id().as_str().to_string();
    let obj = metadata
        .as_object()
        .ok_or_else(|| ProvenanceError::MissingField {
            event_id: event_id.clone(),
            field: "metadata".to_string(),
        })?;

    let agent_id = obj
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProvenanceError::MissingField {
            event_id: event_id.clone(),
            field: "metadata.agent_id".to_string(),
        })?;
    parse_agent_id(event, agent_id)?;

    if matches!(scope, CallScope::Message { .. })
        && obj.get("message_id").and_then(|v| v.as_str()).is_none()
    {
        return Err(ProvenanceError::MissingField {
            event_id,
            field: format!("metadata.message_id ({call_kind})"),
        });
    }

    Ok(())
}

fn base_attrs(event: &ProvEvent) -> HashMap<String, Value> {
    let mut attrs = HashMap::new();
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(event.context_id().as_str().to_string()),
    );
    attrs.insert(
        a2a::EVENT_ID.to_string(),
        Value::String(event.id().as_str().to_string()),
    );
    if let Some(task_id) = event.task_id() {
        attrs.insert(
            a2a::TASK_ID.to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    attrs
}

fn derived_attrs(event: &ProvEvent) -> HashMap<String, Value> {
    let mut attrs = HashMap::new();
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(event.context_id().as_str().to_string()),
    );
    if let Some(task_id) = event.task_id() {
        attrs.insert(
            a2a::TASK_ID.to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    attrs.insert(
        a2a::TIMESTAMP_MS.to_string(),
        Value::Number(event.timestamp_ms().into()),
    );
    attrs
}

fn ensure_task_entity(
    doc: &mut ProvDocument,
    task_id: &TaskId,
    context_id: &ContextId,
    _agent_type: Option<&str>,
) -> ProvEntityId {
    let id = task_entity_id(task_id);
    let mut attrs = doc
        .entity(&id)
        .map(|entity| entity.attributes.clone())
        .unwrap_or_default();
    attrs.insert(
        a2a::TASK_ID.to_string(),
        Value::String(task_id.as_str().to_string()),
    );
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(context_id.as_str().to_string()),
    );
    // agent_id is set during message processing (if message has task_id) or TaskCreated handler
    doc.insert_entity(
        id.clone(),
        Entity {
            prov_type: Some(prov_type::<TaskEntityId>()),
            attributes: attrs,
        },
    );
    id
}

#[allow(clippy::too_many_arguments)]
fn ensure_task_execution_activity(
    doc: &mut ProvDocument,
    task_id: &TaskId,
    context_id: &ContextId,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    _agent_type: Option<&str>,
    agent_registry: &std::collections::HashSet<String>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<ProvActivityId> {
    let id = task_execution_activity_id(task_id);
    let (mut attrs, existing_start, existing_end) = if let Some(activity) = doc.activity(&id) {
        (
            activity.attributes.clone(),
            activity.start_time_ms,
            activity.end_time_ms,
        )
    } else {
        (HashMap::new(), None, None)
    };
    attrs.insert(
        a2a::TASK_ID.to_string(),
        Value::String(task_id.as_str().to_string()),
    );
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(context_id.as_str().to_string()),
    );
    // Extract agent_id from task entity - optional, may not be set yet if TaskCreated hasn't been processed
    let agent_id = task_agent_id(doc, task_id);

    // Look up agent_type from runtime instance agent for display purposes
    if let Some(ref agent_id) = agent_id {
        let agent_instance_id = agent_runtime_instance_id(agent_id);
        if let Some(agent) = doc
            .agent(&agent_instance_id)
            .and_then(|agent| agent.attributes.get(a2a::AGENT_TYPE))
            .and_then(|v| v.as_str())
        {
            attrs.insert(
                a2a::AGENT_TYPE.to_string(),
                Value::String(agent.to_string()),
            );
        }
    }

    let start_time_ms = existing_start.or(start_time_ms);
    let end_time_ms = existing_end.or(end_time_ms);
    doc.insert_activity(
        id.clone(),
        Activity {
            start_time_ms,
            end_time_ms,
            prov_type: Some(prov_type::<TaskExecutionActivityId>()),
            attributes: attrs,
        },
    );
    // Associate with agent if available - if not, association will be added when TaskCreated is processed
    associate_task_execution_agents(doc, &id, agent_id.as_ref(), agent_registry, agent_labels)?;
    Ok(id)
}

fn insert_used(
    doc: &mut ProvDocument,
    activity: ProvActivityId,
    entity: ProvEntityId,
    role: Option<String>,
) {
    let id = doc.blank_node_id("u");
    doc.insert_used(
        id,
        Used {
            activity,
            entity,
            role,
        },
    );
}

fn insert_was_generated_by(
    doc: &mut ProvDocument,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
) {
    let id = doc.blank_node_id("g");
    doc.insert_was_generated_by(
        id,
        WasGeneratedBy {
            entity,
            activity,
            time_ms,
        },
    );
}

fn insert_qualified_generation(
    doc: &mut ProvDocument,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
) {
    let id = doc.blank_node_id("gen");
    doc.insert_qualified_generation(
        id,
        QualifiedGeneration {
            entity,
            activity,
            time_ms,
        },
    );
}

fn insert_was_associated_with(
    doc: &mut ProvDocument,
    activity: ProvActivityId,
    agent: ProvAgentId,
    role: Option<String>,
) {
    let id = doc.blank_node_id("assoc");
    doc.insert_was_associated_with(
        id,
        WasAssociatedWith {
            activity,
            agent,
            role,
        },
    );
}

fn insert_was_derived_from(
    doc: &mut ProvDocument,
    generated_entity: ProvEntityId,
    used_entity: ProvEntityId,
    activity: Option<ProvActivityId>,
    prov_type: Option<String>,
) {
    let id = doc.blank_node_id("d");
    doc.insert_was_derived_from(
        id,
        WasDerivedFrom {
            generated_entity,
            used_entity,
            activity,
            prov_type,
        },
    );
}

/// LLM call activity id: derived from `EventId` to ensure per-call uniqueness.
fn llm_activity_id(event_id: &EventId) -> ProvActivityId {
    ProvActivityId::derived::<LlmCallActivityId>(LlmCallActivityInput { event_id })
}

/// Tool call activity id: derived from `EventId` to ensure per-call uniqueness.
fn tool_activity_id(event_id: &EventId) -> ProvActivityId {
    ProvActivityId::derived::<ToolCallActivityId>(ToolCallActivityInput { event_id })
}

fn llm_prompt_entity_id(event_id: &EventId) -> ProvEntityId {
    ProvEntityId::derived::<LlmPromptEntityId>(LlmPromptEntityInput { event_id })
}

fn tool_args_entity_id(event_id: &EventId) -> ProvEntityId {
    ProvEntityId::derived::<ToolArgsEntityId>(ToolArgsEntityInput { event_id })
}

/// Task entity id: derived from `TaskId` to provide stable task identity.
fn task_entity_id(task_id: &TaskId) -> ProvEntityId {
    ProvEntityId::derived::<TaskEntityId>(TaskEntityInput { task_id })
}

/// Task execution activity id: derived from `TaskId` to group task execution edges.
fn task_execution_activity_id(task_id: &TaskId) -> ProvActivityId {
    ProvActivityId::derived::<TaskExecutionActivityId>(TaskExecutionActivityInput { task_id })
}

/// Agent runtime instance entity id: derived from `AgentId`.
fn agent_runtime_instance_id(agent_id: &AgentId) -> ProvAgentId {
    ProvAgentId::derived::<AgentRuntimeInstanceId>(AgentRuntimeInstanceInput { agent_id })
}

/// Archive entity id: derived from package identity (name@version or hash).
fn archive_entity_id(archive_path: &str) -> ProvEntityId {
    ProvEntityId::derived::<ArchiveEntityId>(ArchiveEntityInput { archive_path })
}

/// Agent boot activity id: derived from `AgentId` (one boot per runtime instance).
fn boot_activity_id(agent_id: &AgentId) -> ProvActivityId {
    ProvActivityId::derived::<AgentBootActivityId>(AgentBootActivityInput { agent_id })
}

/// Runner runtime instance entity id: constant control plane identity.
fn runner_runtime_instance_id() -> ProvAgentId {
    ProvAgentId::constant::<RunnerRuntimeInstanceId>()
}

/// Look up an agent runtime instance in the document.
/// If the instance is not in the document, add it to agent_labels so the graph build can create it
/// (either from agent_registry / AgentBooted, or on first reference e.g. task store operations).
fn get_agent_runtime_instance(
    doc: &ProvDocument,
    agent_id: &AgentId,
    _agent_registry: &std::collections::HashSet<String>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<ProvAgentId> {
    let instance_id = agent_runtime_instance_id(agent_id);
    if doc.agent(&instance_id).is_some() {
        Ok(instance_id)
    } else {
        agent_labels
            .entry(instance_id.as_str().to_string())
            .or_insert_with(|| "AgentRuntimeInstance".to_string());
        Ok(instance_id)
    }
}

fn ensure_runner_runtime_instance(doc: &mut ProvDocument) {
    let id = runner_runtime_instance_id();
    if doc.agent(&id).is_none() {
        let mut attrs = HashMap::new();
        attrs.insert(
            a2a::AGENT_TYPE.to_string(),
            Value::String(agent_types::RUNNER.to_string()),
        );
        doc.insert_agent(
            id,
            Agent {
                prov_type: Some(prov_type::<RunnerRuntimeInstanceId>()),
                attributes: attrs,
            },
        );
    }
}

/// Message entity id: derived from `(ContextId, MessageId)`.
fn message_entity_id(context_id: &ContextId, message_id: &MessageId) -> ProvEntityId {
    ProvEntityId::derived::<MessageEntityId>(MessageEntityInput {
        context_id,
        message_id,
    })
}

/// Message processing activity id: derived from `(ContextId, MessageId)`.
fn message_processing_activity_id(
    context_id: &ContextId,
    message_id: &MessageId,
) -> ProvActivityId {
    ProvActivityId::derived::<MessageProcessingActivityId>(MessageProcessingActivityInput {
        context_id,
        message_id,
    })
}

fn ensure_message_processing_activity(
    doc: &mut ProvDocument,
    context_id: &ContextId,
    message_id: &MessageId,
) -> ProvActivityId {
    let id = message_processing_activity_id(context_id, message_id);
    let mut attrs = doc
        .activity(&id)
        .map(|activity| activity.attributes.clone())
        .unwrap_or_default();
    attrs.insert(
        a2a::CONTEXT_ID.to_string(),
        Value::String(context_id.as_str().to_string()),
    );
    attrs.insert(
        a2a::MESSAGE_ID.to_string(),
        Value::String(message_id.as_str().to_string()),
    );
    doc.insert_activity(
        id.clone(),
        Activity {
            start_time_ms: None,
            end_time_ms: None,
            prov_type: Some(prov_type::<MessageProcessingActivityId>()),
            attributes: attrs,
        },
    );
    id
}

fn attach_message_context(
    doc: &mut ProvDocument,
    event: &ProvEvent,
    activity_id: &ProvActivityId,
    message_id: &MessageId,
    derived_relations: &mut Vec<A2aDerivedRelation>,
) {
    let message_entity_id = message_entity_id(event.context_id(), message_id);
    let mut message_attrs = base_attrs(event);
    message_attrs.insert(
        a2a::MESSAGE_ID.to_string(),
        Value::String(message_id.as_str().to_string()),
    );
    doc.insert_entity(
        message_entity_id.clone(),
        Entity {
            prov_type: Some(prov_type::<MessageEntityId>()),
            attributes: message_attrs,
        },
    );
    insert_used(
        doc,
        activity_id.clone(),
        message_entity_id,
        Some(a2a_roles::INPUT_MESSAGE.to_string()),
    );
    let processing_id = ensure_message_processing_activity(doc, event.context_id(), message_id);
    derived_relations.push(A2aDerivedRelation {
        relation: A2aRelationType::MessageCall,
        from: ProvNodeRef::Activity(processing_id),
        to: ProvNodeRef::Activity(activity_id.clone()),
        attributes: derived_attrs(event),
    });
}

fn scoped_message_id(scope: &CallScope, metadata: &Value) -> Option<MessageId> {
    match scope {
        CallScope::Message { message_id } => Some(message_id.clone()),
        CallScope::Task { .. } => metadata
            .get("message_id")
            .or_else(|| metadata.get(a2a::MESSAGE_ID))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| MessageId::from_external(ExternalId::new(value.to_string()))),
    }
}

fn attach_task_call_context(
    doc: &mut ProvDocument,
    event: &ProvEvent,
    activity_id: &ProvActivityId,
    derived_relations: &mut Vec<A2aDerivedRelation>,
    agent_registry: &std::collections::HashSet<String>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<()> {
    let Some(task_id) = event.task_id() else {
        return Ok(());
    };

    // Try to get agent_id from event metadata (for LLM/Tool calls)
    let agent_id_from_metadata = match event.data() {
        ProvEventData::LlmCallStarted { metadata, .. }
        | ProvEventData::LlmCallCompleted { metadata, .. }
        | ProvEventData::ToolCallStarted { metadata, .. }
        | ProvEventData::ToolCallCompleted { metadata, .. } => metadata
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| parse_agent_id(event, s))
            .transpose()?,
        _ => None,
    };

    // Ensure task entity exists and set agent_id if available from metadata
    let _task_entity = ensure_task_entity(doc, task_id, event.context_id(), None);
    if let Some(agent_id) = agent_id_from_metadata.clone() {
        let task_entity_id = task_entity_id(task_id);
        if let Some(entity) = doc.entity(&task_entity_id) {
            let mut attrs = entity.attributes.clone();
            // Set agent_id on task entity from event metadata if not already set
            if !attrs.contains_key(a2a::AGENT_ID) {
                attrs.insert(
                    a2a::AGENT_ID.to_string(),
                    Value::String(agent_id.as_str().to_string()),
                );
                doc.insert_entity(
                    task_entity_id.clone(),
                    Entity {
                        prov_type: entity.prov_type.clone(),
                        attributes: attrs,
                    },
                );
            }
        }
    }

    let task_execution = ensure_task_execution_activity(
        doc,
        task_id,
        event.context_id(),
        None,
        None,
        None,
        agent_registry,
        agent_labels,
    )?;
    // Associate call with agent - use agent_id from metadata if available, otherwise try task entity
    associate_call_with_agent(
        doc,
        event.context_id(),
        task_id,
        activity_id,
        agent_id_from_metadata.as_ref(),
        agent_registry,
        agent_labels,
    )?;
    derived_relations.push(A2aDerivedRelation {
        relation: A2aRelationType::TaskCall,
        from: ProvNodeRef::Activity(task_execution),
        to: ProvNodeRef::Activity(activity_id.clone()),
        attributes: derived_attrs(event),
    });
    Ok(())
}

// Helper to extract agent_id from task entity - REQUIRED, no fallbacks
fn task_agent_id(doc: &ProvDocument, task_id: &TaskId) -> Option<AgentId> {
    let task_entity = task_entity_id(task_id);
    doc.entity(&task_entity)
        .and_then(|entity| entity.attributes.get(a2a::AGENT_ID))
        .and_then(|v| v.as_str())
        .and_then(|raw| UuidId::parse_str(raw).ok())
        .map(AgentId::from_uuid)
}

fn associate_task_execution_agents(
    doc: &mut ProvDocument,
    task_execution: &ProvActivityId,
    agent_id: Option<&AgentId>,
    agent_registry: &std::collections::HashSet<String>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<()> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };

    let executing_agent_id =
        get_agent_runtime_instance(doc, agent_id, agent_registry, agent_labels)?;
    insert_was_associated_with(
        doc,
        task_execution.clone(),
        executing_agent_id,
        Some(prov_roles::EXECUTING_AGENT.to_string()),
    );

    let invoking_agent_id = runner_runtime_instance_id();
    ensure_runner_runtime_instance(doc);
    insert_was_associated_with(
        doc,
        task_execution.clone(),
        invoking_agent_id,
        Some(prov_roles::INVOKING_AGENT.to_string()),
    );
    Ok(())
}

fn associate_call_with_agent(
    doc: &mut ProvDocument,
    _context_id: &ContextId,
    task_id: &TaskId,
    activity_id: &ProvActivityId,
    agent_id_from_metadata: Option<&AgentId>,
    agent_registry: &std::collections::HashSet<String>,
    agent_labels: &mut HashMap<String, String>,
) -> Result<()> {
    // Try to get agent_id from metadata first, then from task entity
    let agent_id = agent_id_from_metadata.cloned().or_else(|| {
        let task_entity = task_entity_id(task_id);
        doc.entity(&task_entity)
            .and_then(|entity| entity.attributes.get(a2a::AGENT_ID))
            .and_then(|v| v.as_str())
            .and_then(|raw| UuidId::parse_str(raw).ok())
            .map(AgentId::from_uuid)
    });

    // If agent_id is available, associate the call with the agent
    // If not, the association will be added when TaskCreated is processed
    if let Some(agent_id) = agent_id {
        let executing_agent_id =
            get_agent_runtime_instance(doc, &agent_id, agent_registry, agent_labels)?;
        insert_was_associated_with(
            doc,
            activity_id.clone(),
            executing_agent_id,
            Some(prov_roles::EXECUTING_AGENT.to_string()),
        );
    }
    Ok(())
}

fn task_state_entity_id(task_id: &TaskId, timestamp_ms: u64) -> ProvEntityId {
    ProvEntityId::derived::<TaskStateEntityId>(TaskStateEntityInput {
        task_id,
        timestamp_ms,
    })
}

fn task_state_prev_entity_id(task_id: &TaskId, timestamp_ms: u64) -> ProvEntityId {
    ProvEntityId::derived::<TaskStatePrevEntityId>(TaskStatePrevEntityInput {
        task_id,
        timestamp_ms,
    })
}

fn artifact_entity_id(
    task_id: &TaskId,
    artifact_id: &Option<ArtifactId>,
    artifact_type: &Option<String>,
    event_id: &EventId,
) -> ProvEntityId {
    let identity = if let Some(artifact_id) = artifact_id {
        ArtifactIdentity::ById(artifact_id)
    } else if let Some(artifact_type) = artifact_type {
        ArtifactIdentity::ByType {
            task_id,
            artifact_type,
        }
    } else {
        ArtifactIdentity::ByEvent { task_id, event_id }
    };
    match identity {
        ArtifactIdentity::ById(artifact_id) => {
            ProvEntityId::derived::<ArtifactByIdEntityId>(ArtifactByIdEntityInput { artifact_id })
        }
        ArtifactIdentity::ByType {
            task_id,
            artifact_type,
        } => ProvEntityId::derived::<ArtifactByTypeEntityId>(ArtifactByTypeEntityInput {
            task_id,
            artifact_type,
        }),
        ArtifactIdentity::ByEvent { task_id, event_id } => {
            ProvEntityId::derived::<ArtifactByEventEntityId>(ArtifactByEventEntityInput {
                task_id,
                event_id,
            })
        }
    }
}

fn is_terminal_status(status: &str) -> bool {
    let normalized = status.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

fn map_string_map(input: &HashMap<String, String>) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in input {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}
