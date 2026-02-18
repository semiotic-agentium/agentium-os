//! FalkorDB-backed provenance writer.
//!
//! This module transforms normalized W3C PROV + A2A-derived relations into
//! Cypher and persists them into a FalkorDB graph.
//!
//! Key design points:
//! - We use `MERGE` for idempotent upserts by `name`.
//! - Each event is written as a single Cypher query (multiple clauses joined
//!   with `WITH 1 AS _`) to reduce round-trips.
//! - `WITH 1 AS _` resets the variable scope between clauses so we can reuse
//!   short variable names like `n`, `a`, `b`, and `r`.
use crate::cypher_parse::{decode_embedded_json, parse_rows};
use crate::error::{ProvenanceError, Result};
use crate::graph_model::{
    ConversationReadModel, EDGE_WAS_EMITTED_BY, EDGE_WAS_EXECUTED_BY, EDGE_WAS_INVOKED_BY,
    EDGE_WAS_USED_BY, EventGraphKind, EventGraphMapping, GraphNodeLabel, TOOL_CALL_ARGS_EDGE,
    event_kind_from_data, mapping_for_event_data,
};
use crate::normalizer::{
    A2aDerivedRelation, DefaultProvNormalizer, NormalizedProv, ProvNormalizer, validate_event,
};
use crate::store::{
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter, ToolSessionPhase,
};
use crate::types::{
    Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, QualifiedGeneration, Used,
    WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
};
use crate::vocabulary::{
    a2a, a2a_relation_types, a2a_relations, a2a_roles, base_types, message_directions, prov,
    prov_relations, prov_roles, semantic_labels,
};
use async_trait::async_trait;
use baml_rt_observability::metrics;
use falkordb::{FalkorClientBuilder, FalkorConnectionInfo, FalkorValue};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use text_to_cypher::core::execute_cypher_query;

const CLAUSE_SEPARATOR: &str = "\nWITH 1 AS _\n";

#[derive(Debug, Clone)]
pub struct FalkorDbProvenanceConfig {
    /// FalkorDB connection string, e.g. `falkor://127.0.0.1:6379`.
    pub connection: String,
    /// Graph name to store provenance in.
    pub graph: String,
}

impl FalkorDbProvenanceConfig {
    pub fn new(connection: impl Into<String>, graph: impl Into<String>) -> Self {
        Self {
            connection: connection.into(),
            graph: graph.into(),
        }
    }
}

#[derive(Clone)]
pub struct FalkorDbProvenanceWriter {
    config: FalkorDbProvenanceConfig,
    normalizer: Arc<dyn ProvNormalizer>,
}

#[derive(Debug)]
struct MessageRow {
    event_id: String,
    message_id: String,
    direction: String,
    role: String,
    content: Value,
}

impl MessageRow {
    fn parse(row: &[Value]) -> Result<Self> {
        if row.len() < ConversationReadModel::MESSAGE_COLUMN_COUNT {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_EMITTED_BY.to_string(),
                from_label: GraphNodeLabel::Message.as_str().to_string(),
                to_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
            });
        }
        let event_id = value_as_string(&row[0]);
        if event_id.is_empty() {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_EMITTED_BY.to_string(),
                from_label: GraphNodeLabel::Message.as_str().to_string(),
                to_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
            });
        }
        Ok(Self {
            event_id,
            message_id: value_as_string(&row[1]),
            direction: value_as_string(&row[2]),
            role: value_as_string(&row[3]),
            content: row[4].clone(),
        })
    }
}

#[derive(Debug)]
struct ToolCallRow {
    event_id: String,
    tool_name: String,
    metadata: Value,
    args: Value,
    used_role: Option<String>,
    args_prov_type: Option<String>,
    success: Option<bool>,
}

impl ToolCallRow {
    fn parse(row: &[Value]) -> Result<Self> {
        if row.len() < ConversationReadModel::TOOL_COLUMN_COUNT {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_EXECUTED_BY.to_string(),
                from_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
                to_label: GraphNodeLabel::ToolCall.as_str().to_string(),
            });
        }
        let event_id = value_as_string(&row[0]);
        if event_id.is_empty() {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_EXECUTED_BY.to_string(),
                from_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
                to_label: GraphNodeLabel::ToolCall.as_str().to_string(),
            });
        }
        Ok(Self {
            event_id,
            tool_name: value_as_string(&row[1]),
            metadata: decode_embedded_json(&row[2]),
            args: decode_embedded_json(&row[3]),
            used_role: row[4].as_str().map(ToString::to_string),
            args_prov_type: row[5].as_str().map(ToString::to_string),
            success: value_as_bool(&row[6]),
        })
    }

    fn validate(&self) -> Result<()> {
        if self.tool_name.trim().is_empty() {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_EXECUTED_BY.to_string(),
                from_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
                to_label: GraphNodeLabel::ToolCall.as_str().to_string(),
            });
        }
        if self.args.is_null() {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_USED_BY.to_string(),
                from_label: GraphNodeLabel::ToolCall.as_str().to_string(),
                to_label: GraphNodeLabel::ToolArgs.as_str().to_string(),
            });
        }
        if self.used_role.as_deref() != Some(TOOL_CALL_ARGS_EDGE.role_value)
            || self.args_prov_type.as_deref() != Some(TOOL_CALL_ARGS_EDGE.target_type_value)
        {
            return Err(ProvenanceError::InvalidMapping {
                relation: EDGE_WAS_USED_BY.to_string(),
                from_label: GraphNodeLabel::ToolCall.as_str().to_string(),
                to_label: GraphNodeLabel::ToolArgs.as_str().to_string(),
            });
        }
        Ok(())
    }

    fn is_completed(&self) -> bool {
        self.success.is_some()
    }
}

impl FalkorDbProvenanceWriter {
    pub fn new(config: FalkorDbProvenanceConfig) -> Self {
        Self {
            config,
            normalizer: Arc::new(DefaultProvNormalizer::default()),
        }
    }

    pub fn with_normalizer(
        config: FalkorDbProvenanceConfig,
        normalizer: Arc<dyn ProvNormalizer>,
    ) -> Self {
        Self { config, normalizer }
    }

    /// Build a single Cypher query by joining multiple MERGE clauses.
    ///
    /// The `WITH 1 AS _` separator ensures each clause is a new scope so
    /// variable names can be reused without collisions.
    fn build_query(normalized: &NormalizedProv) -> String {
        let mut clauses = Vec::new();
        let mut tool_args_by_event: HashMap<String, (String, String)> = HashMap::new();
        let mut tool_calls_by_event: HashMap<String, (String, String)> = HashMap::new();

        let mut entity_entries: Vec<(&ProvEntityId, &Entity)> =
            normalized.document.entities().collect();
        entity_entries.sort_by_key(|(a, _)| *a);
        let mut entity_labels = HashMap::new();
        for (id, entity) in &entity_entries {
            let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
            entity_labels.insert(id.as_str().to_string(), label);
        }
        for (id, entity) in entity_entries {
            let label = entity_labels
                .get(id.as_str())
                .map(|value| value.as_str())
                .unwrap_or("ProvEntity");
            let props = entity_props(id, entity);
            if label == GraphNodeLabel::ToolArgs.as_str()
                && let Some(event_id) = props.get(a2a::EVENT_ID).and_then(Value::as_str)
            {
                tool_args_by_event.insert(
                    event_id.to_string(),
                    (id.as_str().to_string(), label.to_string()),
                );
            }
            clauses.push(merge_node(label, id.as_str(), &props));
        }

        let mut activity_entries: Vec<(&ProvActivityId, &Activity)> =
            normalized.document.activities().collect();
        activity_entries.sort_by_key(|(a, _)| *a);
        let mut activity_labels = HashMap::new();
        for (id, activity) in &activity_entries {
            let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
            activity_labels.insert(id.as_str().to_string(), label);
        }
        for (id, activity) in activity_entries {
            let label = activity_labels
                .get(id.as_str())
                .map(|value| value.as_str())
                .unwrap_or("ProvActivity");
            let props = activity_props(id, activity);
            if label == GraphNodeLabel::ToolCall.as_str()
                && let Some(event_id) = props.get(a2a::EVENT_ID).and_then(Value::as_str)
            {
                tool_calls_by_event.insert(
                    event_id.to_string(),
                    (id.as_str().to_string(), label.to_string()),
                );
            }
            clauses.push(merge_node(label, id.as_str(), &props));
        }

        let mut agent_entries: Vec<(&ProvAgentId, &Agent)> = normalized.document.agents().collect();
        agent_entries.sort_by_key(|(a, _)| *a);
        let mut agent_labels = HashMap::new();
        for (id, agent) in &agent_entries {
            let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
            agent_labels.insert(id.as_str().to_string(), label);
        }
        for (id, label) in &normalized.agent_labels {
            agent_labels
                .entry(id.clone())
                .or_insert_with(|| label.clone());
        }
        for (id, agent) in agent_entries {
            let label = agent_labels
                .get(id.as_str())
                .map(|value| value.as_str())
                .unwrap_or("ProvAgent");
            let props = agent_props(id, agent);
            clauses.push(merge_node(label, id.as_str(), &props));
        }

        let mut used_entries: Vec<(&String, &Used)> = normalized.document.used().collect();
        used_entries.sort_by_key(|(a, _)| *a);
        for (_, used) in used_entries {
            let props = used_props(used);
            let activity_label = label_for_activity(&activity_labels, used.activity.as_str());
            let entity_label = label_for_entity(&entity_labels, used.entity.as_str());
            let rel_type =
                relation_label(prov_relations::USED, activity_label, entity_label, &props);
            clauses.push(merge_edge(
                activity_label,
                used.activity.as_str(),
                &rel_type,
                entity_label,
                used.entity.as_str(),
                &props,
            ));
        }

        // Enforce canonical ToolCall -> ToolArgs edge so strict readers can
        // reconstruct conversational tool history directly from provenance.
        for (event_id, (tool_call_id, tool_call_label)) in &tool_calls_by_event {
            if let Some((tool_args_id, tool_args_label)) = tool_args_by_event.get(event_id) {
                let mut props = HashMap::new();
                props.insert(
                    prov::BASE_TYPE.to_string(),
                    Value::String("USED".to_string()),
                );
                props.insert(
                    TOOL_CALL_ARGS_EDGE.role_key.to_string(),
                    Value::String(TOOL_CALL_ARGS_EDGE.role_value.to_string()),
                );
                clauses.push(merge_edge(
                    tool_call_label,
                    tool_call_id,
                    TOOL_CALL_ARGS_EDGE.edge_label,
                    tool_args_label,
                    tool_args_id,
                    &props,
                ));
            }
        }
        let mut generated_entries: Vec<(&String, &WasGeneratedBy)> =
            normalized.document.was_generated_by().collect();
        generated_entries.sort_by_key(|(a, _)| *a);
        for (_, generated) in generated_entries {
            let props = was_generated_by_props(generated);
            let entity_label = label_for_ref(
                generated.entity.clone(),
                &entity_labels,
                &activity_labels,
                &agent_labels,
            );
            let activity_label = label_for_activity(&activity_labels, generated.activity.as_str());
            let rel_type = relation_label(
                prov_relations::WAS_GENERATED_BY,
                entity_label,
                activity_label,
                &props,
            );
            clauses.push(merge_edge(
                entity_label,
                generated.entity.id(),
                &rel_type,
                activity_label,
                generated.activity.as_str(),
                &props,
            ));
        }
        let mut qualified_gen_entries: Vec<(&String, &QualifiedGeneration)> =
            normalized.document.qualified_generation().collect();
        qualified_gen_entries.sort_by_key(|(a, _)| *a);
        for (_, generation) in qualified_gen_entries {
            let props = qualified_generation_props(generation);
            let entity_label = label_for_ref(
                generation.entity.clone(),
                &entity_labels,
                &activity_labels,
                &agent_labels,
            );
            let activity_label = label_for_activity(&activity_labels, generation.activity.as_str());
            let rel_type = relation_label(
                prov_relations::QUALIFIED_GENERATION,
                entity_label,
                activity_label,
                &props,
            );
            clauses.push(merge_edge(
                entity_label,
                generation.entity.id(),
                &rel_type,
                activity_label,
                generation.activity.as_str(),
                &props,
            ));
        }
        let mut assoc_entries: Vec<(&String, &WasAssociatedWith)> =
            normalized.document.was_associated_with().collect();
        assoc_entries.sort_by_key(|(a, _)| *a);
        for (_, assoc) in assoc_entries {
            let props = was_associated_with_props(assoc);
            let activity_label = label_for_activity(&activity_labels, assoc.activity.as_str());
            let agent_label = label_for_agent(&agent_labels, assoc.agent.as_str());
            let rel_type = relation_label(
                prov_relations::WAS_ASSOCIATED_WITH,
                activity_label,
                agent_label,
                &props,
            );
            clauses.push(merge_edge(
                activity_label,
                assoc.activity.as_str(),
                &rel_type,
                agent_label,
                assoc.agent.as_str(),
                &props,
            ));
        }
        let mut derived_entries: Vec<(&String, &WasDerivedFrom)> =
            normalized.document.was_derived_from().collect();
        derived_entries.sort_by_key(|(a, _)| *a);
        for (_, derived) in derived_entries {
            let props = was_derived_from_props(derived);
            let generated_label =
                label_for_entity(&entity_labels, derived.generated_entity.as_str());
            let used_label = label_for_entity(&entity_labels, derived.used_entity.as_str());
            let rel_type = relation_label(
                prov_relations::WAS_DERIVED_FROM,
                generated_label,
                used_label,
                &props,
            );
            clauses.push(merge_edge(
                generated_label,
                derived.generated_entity.as_str(),
                &rel_type,
                used_label,
                derived.used_entity.as_str(),
                &props,
            ));
        }

        for relation in &normalized.derived_relations {
            clauses.push(merge_derived_relation(
                relation,
                &entity_labels,
                &activity_labels,
                &agent_labels,
            ));
        }

        if clauses.is_empty() {
            return String::new();
        }

        clauses.join(CLAUSE_SEPARATOR)
    }
}

fn value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

fn mapping_nodes_for_event<'a>(
    normalized: &'a NormalizedProv,
    mapping: &EventGraphMapping,
    event_id: &str,
    event: &crate::events::ProvEvent,
) -> Vec<(&'a HashMap<String, Value>, String)> {
    let mut out = Vec::new();
    for (id, entity) in normalized.document.entities() {
        let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
        if label == mapping.primary_node.as_str()
            && entity.attributes.get(a2a::EVENT_ID).and_then(Value::as_str) == Some(event_id)
        {
            out.push((&entity.attributes, id.as_str().to_string()));
        }
    }
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        if label == mapping.primary_node.as_str()
            && activity
                .attributes
                .get(a2a::EVENT_ID)
                .and_then(Value::as_str)
                == Some(event_id)
        {
            out.push((&activity.attributes, id.as_str().to_string()));
        }
    }
    for (id, agent) in normalized.document.agents() {
        let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
        if label == mapping.primary_node.as_str()
            && agent.attributes.get(a2a::EVENT_ID).and_then(Value::as_str) == Some(event_id)
        {
            out.push((&agent.attributes, id.as_str().to_string()));
        }
    }
    if !out.is_empty() {
        return out;
    }

    // Some long-lived activity/entity nodes are keyed by task identity rather than
    // per-event IDs (e.g. A2ATaskExecution). Fall back to task-scoped matching.
    let Some(task_id) = event.task_id().map(|t| t.as_str()) else {
        return out;
    };
    for (id, entity) in normalized.document.entities() {
        let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
        if label == mapping.primary_node.as_str()
            && entity.attributes.get(a2a::TASK_ID).and_then(Value::as_str) == Some(task_id)
        {
            out.push((&entity.attributes, id.as_str().to_string()));
        }
    }
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        if label == mapping.primary_node.as_str()
            && activity
                .attributes
                .get(a2a::TASK_ID)
                .and_then(Value::as_str)
                == Some(task_id)
        {
            out.push((&activity.attributes, id.as_str().to_string()));
        }
    }
    for (id, agent) in normalized.document.agents() {
        let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
        if label == mapping.primary_node.as_str()
            && agent.attributes.get(a2a::TASK_ID).and_then(Value::as_str) == Some(task_id)
        {
            out.push((&agent.attributes, id.as_str().to_string()));
        }
    }
    out
}

fn validate_required_properties(
    mapping: &EventGraphMapping,
    attrs: &HashMap<String, Value>,
    event_id: &str,
) -> Result<()> {
    for key in mapping.required_properties {
        let value = attrs
            .get(*key)
            .ok_or_else(|| ProvenanceError::MissingField {
                event_id: event_id.to_string(),
                field: key.to_string(),
            })?;
        if !value_present(value) {
            return Err(ProvenanceError::MissingField {
                event_id: event_id.to_string(),
                field: key.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_tool_args_edge(normalized: &NormalizedProv, event_id: &str) -> Result<()> {
    let tool_call_id = normalized
        .document
        .activities()
        .find_map(|(id, activity)| {
            let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
            if label == GraphNodeLabel::ToolCall.as_str()
                && activity
                    .attributes
                    .get(a2a::EVENT_ID)
                    .and_then(Value::as_str)
                    == Some(event_id)
            {
                Some(id.as_str().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| ProvenanceError::InvalidMapping {
            relation: EDGE_WAS_EXECUTED_BY.to_string(),
            from_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
            to_label: GraphNodeLabel::ToolCall.as_str().to_string(),
        })?;

    let tool_args_id = normalized
        .document
        .entities()
        .find_map(|(id, entity)| {
            let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
            if label == GraphNodeLabel::ToolArgs.as_str()
                && entity.attributes.get(a2a::EVENT_ID).and_then(Value::as_str) == Some(event_id)
            {
                Some(id.as_str().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| ProvenanceError::InvalidMapping {
            relation: EDGE_WAS_USED_BY.to_string(),
            from_label: GraphNodeLabel::ToolCall.as_str().to_string(),
            to_label: GraphNodeLabel::ToolArgs.as_str().to_string(),
        })?;

    let has_required_used = normalized.document.used().any(|(_, used)| {
        used.activity.as_str() == tool_call_id
            && used.entity.as_str() == tool_args_id
            && used.role.as_deref() == Some(TOOL_CALL_ARGS_EDGE.role_value)
    });
    if !has_required_used {
        return Err(ProvenanceError::InvalidMapping {
            relation: EDGE_WAS_USED_BY.to_string(),
            from_label: GraphNodeLabel::ToolCall.as_str().to_string(),
            to_label: GraphNodeLabel::ToolArgs.as_str().to_string(),
        });
    }

    Ok(())
}

fn validate_agent_boot_archive(normalized: &NormalizedProv, event_id: &str) -> Result<()> {
    let has_archive = normalized.document.entities().any(|(_, entity)| {
        let label = label_from_prov_type(entity.prov_type.as_deref(), base_types::ENTITY);
        label == GraphNodeLabel::AgentArchive.as_str()
            && entity.attributes.get(a2a::EVENT_ID).and_then(Value::as_str) == Some(event_id)
            && entity
                .attributes
                .get(a2a::ARCHIVE_PATH)
                .is_some_and(value_present)
    });
    if !has_archive {
        return Err(ProvenanceError::MissingField {
            event_id: event_id.to_string(),
            field: a2a::ARCHIVE_PATH.to_string(),
        });
    }
    Ok(())
}

fn validate_llm_prompt_edge(normalized: &NormalizedProv, event_id: &str) -> Result<()> {
    let llm_call_id = normalized
        .document
        .activities()
        .find_map(|(id, activity)| {
            let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
            if label == GraphNodeLabel::LlmCall.as_str()
                && activity
                    .attributes
                    .get(a2a::EVENT_ID)
                    .and_then(Value::as_str)
                    == Some(event_id)
            {
                Some(id.as_str().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| ProvenanceError::InvalidMapping {
            relation: EDGE_WAS_INVOKED_BY.to_string(),
            from_label: GraphNodeLabel::MessageProcessing.as_str().to_string(),
            to_label: GraphNodeLabel::LlmCall.as_str().to_string(),
        })?;

    let prompt_id = normalized
        .document
        .entities()
        .find_map(|(id, entity)| {
            let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
            if label == GraphNodeLabel::LlmPrompt.as_str()
                && entity.attributes.get(a2a::EVENT_ID).and_then(Value::as_str) == Some(event_id)
            {
                Some(id.as_str().to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| ProvenanceError::MissingField {
            event_id: event_id.to_string(),
            field: a2a::PROMPT.to_string(),
        })?;

    let has_prompt_value = normalized.document.entities().any(|(id, entity)| {
        id.as_str() == prompt_id
            && entity
                .attributes
                .get(a2a::PROMPT)
                .is_some_and(value_present)
    });
    if !has_prompt_value {
        return Err(ProvenanceError::MissingField {
            event_id: event_id.to_string(),
            field: a2a::PROMPT.to_string(),
        });
    }

    let has_prompt_used = normalized.document.used().any(|(_, used)| {
        used.activity.as_str() == llm_call_id
            && used.entity.as_str() == prompt_id
            && used.role.as_deref() == Some(a2a_roles::PROMPT)
    });
    if !has_prompt_used {
        return Err(ProvenanceError::InvalidMapping {
            relation: EDGE_WAS_USED_BY.to_string(),
            from_label: GraphNodeLabel::LlmCall.as_str().to_string(),
            to_label: GraphNodeLabel::LlmPrompt.as_str().to_string(),
        });
    }

    Ok(())
}

fn validate_strict_event_mapping(
    normalized: &NormalizedProv,
    event: &crate::events::ProvEvent,
) -> Result<()> {
    let mapping = mapping_for_event_data(event.data());
    let event_id = event.id().to_string();
    let nodes = mapping_nodes_for_event(normalized, mapping, &event_id, event);
    if nodes.is_empty() {
        return Err(ProvenanceError::InvalidMapping {
            relation: "EVENT_TO_NODE".to_string(),
            from_label: format!("{:?}", mapping.kind),
            to_label: mapping.primary_node.as_str().to_string(),
        });
    }

    let mut property_valid = false;
    for (attrs, _) in &nodes {
        if validate_required_properties(mapping, attrs, &event_id).is_ok() {
            property_valid = true;
            break;
        }
    }
    if !property_valid {
        validate_required_properties(mapping, nodes[0].0, &event_id)?;
    }

    match mapping.kind {
        EventGraphKind::LlmCallStarted | EventGraphKind::LlmCallCompleted => {
            validate_llm_prompt_edge(normalized, &event_id)?;
        }
        EventGraphKind::ToolCallStarted | EventGraphKind::ToolCallCompleted => {
            validate_tool_args_edge(normalized, &event_id)?;
        }
        EventGraphKind::AgentBooted => {
            validate_agent_boot_archive(normalized, &event_id)?;
        }
        _ => {}
    }
    Ok(())
}

#[async_trait]
impl ProvenanceWriter for FalkorDbProvenanceWriter {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        let kind = event_kind_from_data(event.data());
        let event_kind_str = format!("{:?}", kind);
        let span = tracing::debug_span!(
            "baml_rt.provenance_write",
            event_kind = ?kind,
        );
        let _guard = span.enter();
        let start = Instant::now();
        validate_event(&event)?;
        let normalized = self.normalizer.normalize(&event)?;
        validate_strict_event_mapping(&normalized, &event)?;
        let query = Self::build_query(&normalized);
        if query.is_empty() {
            metrics::record_provenance_write(&event_kind_str, "success", start.elapsed());
            return Ok(());
        }
        let result =
            execute_cypher_query(&query, &self.config.graph, &self.config.connection, false).await;
        let duration = start.elapsed();
        let result_str = if result.is_ok() { "success" } else { "error" };
        metrics::record_provenance_write(&event_kind_str, result_str, duration);
        result.map(|_| ()).map_err(Into::into)
    }
}

#[async_trait]
impl ProvenanceContextReader for FalkorDbProvenanceWriter {
    async fn context_messages(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        let context = context_id.as_str();
        let message_query = ConversationReadModel::message_query(context);
        let typed_rows = execute_read_rows(
            &message_query,
            &self.config.graph,
            &self.config.connection,
            true,
        )
        .await?;

        let mut messages: Vec<ProvenanceContextMessage> = typed_rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(falkor_value_to_json)
                    .collect::<Vec<Value>>()
            })
            .filter_map(|row| MessageRow::parse(&row).ok())
            .map(|message_row| {
                let role = if message_row.direction == message_directions::RECEIVED {
                    "user".to_string()
                } else {
                    message_row.role
                };
                ProvenanceContextMessage {
                    message_id: message_row.message_id,
                    timestamp_ms: event_id_counter(&message_row.event_id),
                    role,
                    content: vec![normalize_message_content(&message_row.content)],
                }
            })
            .collect();
        messages.retain(|m| !m.content.iter().all(|part| part.trim().is_empty()));
        messages.sort_by_key(|m| m.timestamp_ms);
        if let Some(limit) = limit {
            if limit == 0 {
                return Ok(Vec::new());
            }
            if messages.len() > limit {
                messages = messages.split_off(messages.len() - limit);
            }
        }
        Ok(messages)
    }

    async fn conversation_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let context = context_id.as_str();
        let message_query = ConversationReadModel::message_query(context);
        let tool_query = ConversationReadModel::tool_query(context);

        tracing::debug!(
            context_id = context,
            message_query = %message_query,
            tool_query = %tool_query,
            "Provenance conversation_context: executing queries"
        );

        let message_typed = execute_read_rows(
            &message_query,
            &self.config.graph,
            &self.config.connection,
            true,
        )
        .await?;
        let tool_typed = execute_read_rows(
            &tool_query,
            &self.config.graph,
            &self.config.connection,
            true,
        )
        .await?;

        let message_parsed: Vec<Vec<Value>> = message_typed
            .into_iter()
            .map(|row| row.into_iter().map(falkor_value_to_json).collect())
            .collect();
        let tool_parsed: Vec<Vec<Value>> = tool_typed
            .into_iter()
            .map(|row| row.into_iter().map(falkor_value_to_json).collect())
            .collect();

        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in message_parsed {
            let message_row = MessageRow::parse(&row)?;
            let direction = message_row.direction;
            let role = if direction == message_directions::RECEIVED {
                "user".to_string()
            } else {
                message_row.role
            };
            let content = normalize_message_content(&message_row.content);
            if content.trim().is_empty() {
                continue;
            }
            items.push(ProvenanceConversationContextItem {
                timestamp_ms: event_id_counter(&message_row.event_id),
                event_id: message_row.event_id,
                role,
                content: Value::String(content),
                source: "message".to_string(),
            });
        }

        for row in tool_parsed {
            let tool_row = ToolCallRow::parse(&row)?;
            tool_row.validate()?;
            if !tool_row.is_completed() {
                continue;
            }
            let phase = ToolSessionPhase::from_metadata(&tool_row.metadata);
            let result = tool_row
                .metadata
                .get("result")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let include_call = !matches!(
                phase,
                ToolSessionPhase::Open | ToolSessionPhase::Finish | ToolSessionPhase::Abort
            ) && (!is_empty_object(&tool_row.args)
                || has_meaningful_result(&result));

            if include_call {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_counter(&tool_row.event_id),
                    event_id: tool_row.event_id.clone(),
                    role: "assistant".to_string(),
                    content: serde_json::json!({
                        "tool_call": {
                            "name": tool_row.tool_name,
                            "args": tool_row.args,
                            "fsm_phase": phase.label()
                        }
                    }),
                    source: "tool_call".to_string(),
                });
            }

            if include_call && has_meaningful_result(&result) {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_counter(&tool_row.event_id),
                    event_id: tool_row.event_id,
                    role: "tool".to_string(),
                    content: serde_json::json!({
                        "tool_name": tool_row.tool_name,
                        "fsm_phase": phase.label(),
                        "result": result
                    }),
                    source: "tool_result".to_string(),
                });
            }
        }

        tracing::debug!(
            context_id = context,
            total_items = items.len(),
            message_items = items.iter().filter(|i| i.source == "message").count(),
            tool_call_items = items.iter().filter(|i| i.source == "tool_call").count(),
            tool_result_items = items.iter().filter(|i| i.source == "tool_result").count(),
            "Provenance conversation_context: assembled items"
        );

        items.sort_by_key(|item| {
            (
                item.timestamp_ms,
                event_id_counter(&item.event_id),
                item.source.clone(),
            )
        });

        if let Some(limit) = limit {
            if limit == 0 {
                return Ok(Vec::new());
            }
            if items.len() > limit {
                items = items.split_off(items.len() - limit);
            }
        }

        Ok(items)
    }
}

/// Build an A2A-derived relation edge between two PROV nodes.
fn merge_derived_relation(
    relation: &A2aDerivedRelation,
    entity_labels: &HashMap<String, String>,
    activity_labels: &HashMap<String, String>,
    agent_labels: &HashMap<String, String>,
) -> String {
    let props = relation_props(relation);
    let from_label = label_for_ref(
        relation.from.clone(),
        entity_labels,
        activity_labels,
        agent_labels,
    );
    let to_label = label_for_ref(
        relation.to.clone(),
        entity_labels,
        activity_labels,
        agent_labels,
    );
    let rel_type = derived_relation_label(relation, from_label, to_label, &props);
    merge_edge(
        from_label,
        relation.from.id(),
        &rel_type,
        to_label,
        relation.to.id(),
        &props,
    )
}

/// Convert a PROV Entity into a Cypher property map (including `name`).
fn entity_props(id: &ProvEntityId, entity: &Entity) -> HashMap<String, Value> {
    let mut props = entity.attributes.clone();
    insert_type(&mut props, entity.prov_type.as_ref());
    insert_base_type(&mut props, "ProvEntity");
    insert_id_props(&mut props, id.as_str());
    props
}

fn activity_props(id: &ProvActivityId, activity: &Activity) -> HashMap<String, Value> {
    let mut props = activity.attributes.clone();
    if let Some(start_time_ms) = activity.start_time_ms {
        props.insert(
            prov::START_TIME.to_string(),
            Value::Number(start_time_ms.into()),
        );
    }
    if let Some(end_time_ms) = activity.end_time_ms {
        props.insert(
            prov::END_TIME.to_string(),
            Value::Number(end_time_ms.into()),
        );
    }
    insert_type(&mut props, activity.prov_type.as_ref());
    insert_base_type(&mut props, "ProvActivity");
    insert_id_props(&mut props, id.as_str());
    props
}

fn agent_props(id: &ProvAgentId, agent: &Agent) -> HashMap<String, Value> {
    let mut props = agent.attributes.clone();
    insert_type(&mut props, agent.prov_type.as_ref());
    insert_base_type(&mut props, "ProvAgent");
    insert_id_props(&mut props, id.as_str());
    props
}

/// Relation properties for `USED`.
fn used_props(used: &Used) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(prov_relations::USED.to_string()),
    );
    if let Some(role) = &used.role {
        props.insert(prov::ROLE.to_string(), Value::String(role.clone()));
    }
    props
}

fn was_generated_by_props(generated: &WasGeneratedBy) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(prov_relations::WAS_GENERATED_BY.to_string()),
    );
    if let Some(time_ms) = generated.time_ms {
        props.insert(prov::TIME.to_string(), Value::Number(time_ms.into()));
    }
    props
}

fn qualified_generation_props(generation: &QualifiedGeneration) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(prov_relations::QUALIFIED_GENERATION.to_string()),
    );
    if let Some(time_ms) = generation.time_ms {
        props.insert(prov::TIME.to_string(), Value::Number(time_ms.into()));
    }
    props
}

fn was_associated_with_props(assoc: &WasAssociatedWith) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(prov_relations::WAS_ASSOCIATED_WITH.to_string()),
    );
    if let Some(role) = &assoc.role {
        props.insert(prov::ROLE.to_string(), Value::String(role.clone()));
    }
    props
}

fn was_derived_from_props(derived: &WasDerivedFrom) -> HashMap<String, Value> {
    let mut props = HashMap::new();
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(prov_relations::WAS_DERIVED_FROM.to_string()),
    );
    if let Some(activity) = &derived.activity {
        props.insert(
            prov::ACTIVITY.to_string(),
            Value::String(activity.to_string()),
        );
    }
    if let Some(prov_type) = &derived.prov_type {
        props.insert(prov::TYPE.to_string(), Value::String(prov_type.clone()));
    }
    props
}

fn insert_base_type(props: &mut HashMap<String, Value>, base_type: &str) {
    props.insert(
        prov::BASE_TYPE.to_string(),
        Value::String(base_type.to_string()),
    );
}

fn relation_props(relation: &A2aDerivedRelation) -> HashMap<String, Value> {
    let mut props = relation.attributes.clone();
    // FalkorDB supports relationship properties; we persist event context on derived edges.
    props.insert(
        a2a::RELATION.to_string(),
        Value::String(relation.relation.as_str().to_string()),
    );
    props.insert(
        a2a::FROM.to_string(),
        Value::String(relation.from.id().to_string()),
    );
    props.insert(
        a2a::TO.to_string(),
        Value::String(relation.to.id().to_string()),
    );
    props
}

fn insert_type(props: &mut HashMap<String, Value>, prov_type: Option<&String>) {
    if let Some(prov_type) = prov_type {
        props.insert(prov::TYPE.to_string(), Value::String(prov_type.clone()));
    }
}

fn label_from_prov_type(prov_type: Option<&str>, fallback: &str) -> String {
    let raw = prov_type
        .and_then(|value| value.split(':').next_back())
        .unwrap_or(fallback);
    sanitize_label(raw, fallback)
}

fn sanitize_label(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return fallback.to_string();
    }
    let first = out.chars().next().unwrap_or('_');
    if first.is_ascii_alphabetic() || first == '_' {
        out
    } else {
        format!("L_{}", out)
    }
}

fn relation_label(
    base: &str,
    from_label: &str,
    to_label: &str,
    props: &HashMap<String, Value>,
) -> String {
    let semantic = match base {
        prov_relations::USED => semantic_used(from_label, to_label, props),
        prov_relations::WAS_GENERATED_BY => semantic_generated_by(from_label, to_label),
        prov_relations::WAS_ASSOCIATED_WITH => semantic_associated_with(props),
        prov_relations::WAS_DERIVED_FROM => semantic_derived_from(props),
        _ => None,
    };
    let label = semantic.unwrap_or(base);
    sanitize_label(label, base)
}

fn semantic_used(
    from_label: &str,
    _to_label: &str,
    props: &HashMap<String, Value>,
) -> Option<&'static str> {
    let role = props.get(prov::ROLE).and_then(Value::as_str);
    match role {
        Some(a2a_roles::INPUT_MESSAGE) => Some(match from_label {
            label if label == GraphNodeLabel::TaskExecution.as_str() => {
                semantic_labels::WAS_SPAWNED_BY
            }
            label if label == GraphNodeLabel::MessageProcessing.as_str() => {
                semantic_labels::WAS_RECEIVED_BY
            }
            label if label == GraphNodeLabel::LlmCall.as_str() => semantic_labels::WAS_CONSUMED_BY,
            label if label == GraphNodeLabel::ToolCall.as_str() => semantic_labels::WAS_CONSUMED_BY,
            _ => semantic_labels::WAS_USED_BY,
        }),
        Some(a2a_roles::TASK_STATE) => Some(semantic_labels::WAS_UPDATED_BY),
        Some(a2a_roles::PROMPT) => Some(semantic_labels::WAS_USED_BY),
        Some(a2a_roles::ARGS) => Some(semantic_labels::WAS_USED_BY),
        Some(a2a_roles::ARCHIVE) => Some(semantic_labels::WAS_BOOTSTRAPPED_BY),
        _ => None,
    }
}

fn semantic_associated_with(props: &HashMap<String, Value>) -> Option<&'static str> {
    let role = props.get(prov::ROLE).and_then(Value::as_str);
    match role {
        Some(role) if role == prov_roles::EXECUTING_AGENT => Some(semantic_labels::WAS_EXECUTED_BY),
        Some(role) if role == prov_roles::INVOKING_AGENT => Some(semantic_labels::WAS_INVOKED_BY),
        Some(role) if role == prov_roles::CALLING_AGENT => Some(semantic_labels::WAS_CALLED_BY),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
enum GeneratedByPair {
    MessageProcessing,
    ArtifactTaskExecution,
    TaskTaskExecution,
    AgentRuntimeInstanceBoot,
}

impl GeneratedByPair {
    fn from_labels(from_label: &str, to_label: &str) -> Option<Self> {
        match (from_label, to_label) {
            (f, t)
                if f == GraphNodeLabel::Message.as_str()
                    && t == GraphNodeLabel::MessageProcessing.as_str() =>
            {
                Some(Self::MessageProcessing)
            }
            (f, t)
                if f == GraphNodeLabel::Artifact.as_str()
                    && t == GraphNodeLabel::TaskExecution.as_str() =>
            {
                Some(Self::ArtifactTaskExecution)
            }
            (f, t)
                if f == GraphNodeLabel::Task.as_str()
                    && t == GraphNodeLabel::TaskExecution.as_str() =>
            {
                Some(Self::TaskTaskExecution)
            }
            (f, t)
                if f == GraphNodeLabel::AgentRuntimeInstance.as_str()
                    && t == GraphNodeLabel::AgentBoot.as_str() =>
            {
                Some(Self::AgentRuntimeInstanceBoot)
            }
            _ => None,
        }
    }
}

fn semantic_generated_by(from_label: &str, to_label: &str) -> Option<&'static str> {
    let pair = GeneratedByPair::from_labels(from_label, to_label)?;
    let label = match pair {
        GeneratedByPair::MessageProcessing => semantic_labels::WAS_EMITTED_BY,
        GeneratedByPair::ArtifactTaskExecution => semantic_labels::WAS_GENERATED_BY,
        GeneratedByPair::TaskTaskExecution => semantic_labels::WAS_CREATED_BY,
        GeneratedByPair::AgentRuntimeInstanceBoot => semantic_labels::WAS_SPAWNED_BY,
    };
    Some(label)
}

fn semantic_derived_from(props: &HashMap<String, Value>) -> Option<&'static str> {
    let prov_type = props.get(prov::TYPE).and_then(Value::as_str);
    match prov_type {
        Some(a2a_relation_types::STATUS_TRANSITION) => Some(semantic_labels::WAS_TRANSITIONED_FROM),
        _ => None,
    }
}

fn derived_relation_label(
    relation: &A2aDerivedRelation,
    _from_label: &str,
    to_label: &str,
    props: &HashMap<String, Value>,
) -> String {
    let semantic = match relation.relation.as_str() {
        a2a_relations::TASK_CALL => match to_label {
            label if label == GraphNodeLabel::LlmCall.as_str() => {
                Some(semantic_labels::WAS_INVOKED_BY)
            }
            label if label == GraphNodeLabel::ToolCall.as_str() => {
                Some(semantic_labels::WAS_EXECUTED_BY)
            }
            _ => None,
        },
        a2a_relations::MESSAGE_CALL => match to_label {
            label if label == GraphNodeLabel::LlmCall.as_str() => {
                Some(semantic_labels::WAS_INVOKED_BY)
            }
            label if label == GraphNodeLabel::ToolCall.as_str() => {
                Some(semantic_labels::WAS_EXECUTED_BY)
            }
            _ => None,
        },
        a2a_relations::TASK_MESSAGE => match props.get(a2a::DIRECTION).and_then(Value::as_str) {
            Some(message_directions::RECEIVED) => Some(semantic_labels::WAS_SPAWNED_BY),
            Some(message_directions::SENT) => Some(semantic_labels::WAS_EMITTED_BY),
            _ => Some(semantic_labels::WAS_RELATED_TO),
        },
        a2a_relations::TASK_ARTIFACT => Some(semantic_labels::WAS_GENERATED_BY),
        a2a_relations::TASK_STATUS_TRANSITION => Some(semantic_labels::WAS_TRANSITIONED_TO),
        _ => None,
    };
    let label = semantic.unwrap_or(relation.relation.as_str());
    sanitize_label(label, relation.relation.as_str())
}

fn label_for_entity<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or(base_types::ENTITY)
}

fn label_for_activity<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or(base_types::ACTIVITY)
}

fn label_for_agent<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or(base_types::AGENT)
}

fn label_for_ref<'a>(
    reference: crate::types::ProvNodeRef,
    entity_labels: &'a HashMap<String, String>,
    activity_labels: &'a HashMap<String, String>,
    agent_labels: &'a HashMap<String, String>,
) -> &'a str {
    match reference {
        crate::types::ProvNodeRef::Entity(id) => label_for_entity(entity_labels, id.as_str()),
        crate::types::ProvNodeRef::Activity(id) => label_for_activity(activity_labels, id.as_str()),
        // Agent nodes can appear in derived relations when needed.
        // If no agent label exists, fall back to ProvAgent.
        // (Currently unused, but kept for completeness.)
        // Note: derived relations today only point to entities/activities.
        //
        // This branch allows future agent-derived relations without schema changes.
        // It does not emit new nodes by itself.
        // - If agent_labels has no entry, it returns "ProvAgent".
        crate::types::ProvNodeRef::Agent(id) => label_for_agent(agent_labels, id.as_str()),
    }
}

/// Insert stable identifiers used by upsert logic.
fn insert_id_props(props: &mut HashMap<String, Value>, id: &str) {
    props.insert("name".to_string(), Value::String(id.to_string()));
}

/// Create an idempotent node upsert.
///
/// `MERGE` will either match an existing node (same `name`) or create it.
/// `SET n += {props}` then adds/updates properties without clearing others.
fn merge_node(label: &str, id: &str, props: &HashMap<String, Value>) -> String {
    let id_value = Value::String(id.to_string());
    format!(
        "MERGE (n:{label} {{name: {name}}}) SET n += {props}",
        name = cypher_value(&id_value),
        props = cypher_map(props)
    )
}

/// Create an idempotent edge upsert between two nodes.
///
/// We `MERGE` both nodes (by `name`) and then `MERGE` the relationship.
/// This avoids `MATCH` after an updating clause and keeps the clause atomic.
fn merge_edge(
    from_label: &str,
    from_id: &str,
    rel_type: &str,
    to_label: &str,
    to_id: &str,
    props: &HashMap<String, Value>,
) -> String {
    let from_value = Value::String(from_id.to_string());
    let to_value = Value::String(to_id.to_string());
    let base = format!(
        "MERGE (a:{from_label} {{name: {from_id}}}) MERGE (b:{to_label} {{name: {to_id}}}) MERGE (a)-[r:{rel_type}]->(b)",
        from_id = cypher_value(&from_value),
        to_id = cypher_value(&to_value)
    );
    if props.is_empty() {
        base
    } else {
        format!("{base} SET r += {}", cypher_map(props))
    }
}

/// Render a JSON map as a Cypher map literal with stable key ordering.
fn cypher_map(map: &HashMap<String, Value>) -> String {
    if map.is_empty() {
        return "{}".to_string();
    }
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by_key(|(a, _)| *a);
    let mut parts = Vec::new();
    for (key, value) in entries {
        parts.push(format!("{}: {}", cypher_key(key), cypher_value(value)));
    }
    format!("{{{}}}", parts.join(", "))
}

fn cypher_key(key: &str) -> String {
    if is_safe_identifier(key) {
        key.to_string()
    } else {
        format!("`{}`", key.replace('`', "``"))
    }
}

/// Determine if a key can be used without backticks in Cypher.
fn is_safe_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn cypher_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(values) => {
            if values.iter().all(is_primitive_value) {
                let mut parts = Vec::new();
                for value in values {
                    parts.push(cypher_value(value));
                }
                format!("[{}]", parts.join(", "))
            } else {
                let json = serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string());
                json_string_literal(&json)
            }
        }
        Value::Object(map) => {
            let json = serde_json::to_string(map).unwrap_or_else(|_| "{}".to_string());
            json_string_literal(&json)
        }
    }
}

fn is_primitive_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn json_string_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Execute a read-only (or read-write) Cypher query and return typed rows.
///
/// This mirrors `text_to_cypher::core::execute_cypher_query` but skips the
/// lossy text formatter, returning `Vec<Vec<FalkorValue>>` directly.
async fn execute_read_rows(
    query: &str,
    graph_name: &str,
    falkordb_connection: &str,
    read_only: bool,
) -> std::result::Result<Vec<Vec<FalkorValue>>, Box<dyn std::error::Error + Send + Sync>> {
    let connection_info: FalkorConnectionInfo = falkordb_connection
        .try_into()
        .map_err(|e| format!("Invalid connection info: {e}"))?;

    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .build()
        .await
        .map_err(|e| format!("Failed to build FalkorDB client: {e}"))?;

    let graph_name = graph_name.to_string();
    let query = query.to_string();

    tokio::task::spawn_blocking(move || {
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| format!("Failed to create runtime: {e}"))?;
        rt.block_on(async {
            let mut graph = client.select_graph(&graph_name);
            let query_result = if read_only {
                graph
                    .ro_query(&query)
                    .execute()
                    .await
                    .map_err(|e| format!("Read query execution failed: {e}"))?
            } else {
                graph
                    .query(&query)
                    .execute()
                    .await
                    .map_err(|e| format!("Query execution failed: {e}"))?
            };
            // LazyResultSet is an iterator over Vec<FalkorValue>; collect eagerly.
            let rows: Vec<Vec<FalkorValue>> = query_result.data.collect();
            Ok(rows)
        })
    })
    .await
    .map_err(|e| format!("Failed to execute blocking task: {e}"))?
}

/// Convert a single `FalkorValue` into a `serde_json::Value`.
///
/// This is the typed replacement for the lossy text round-trip through
/// `format_query_records` → `parse_graph_snapshot`.
fn falkor_value_to_json(fv: FalkorValue) -> Value {
    match fv {
        FalkorValue::String(s) => Value::String(s),
        FalkorValue::I64(n) => Value::Number(serde_json::Number::from(n)),
        FalkorValue::F64(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        FalkorValue::Bool(b) => Value::Bool(b),
        FalkorValue::None => Value::Null,
        FalkorValue::Array(items) => {
            Value::Array(items.into_iter().map(falkor_value_to_json).collect())
        }
        FalkorValue::Map(map) => {
            let obj: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, falkor_value_to_json(v)))
                .collect();
            Value::Object(obj)
        }
        // Node and Edge don't implement Serialize in falkordb 0.2.
        // These variants should never appear in conversation read queries
        // (which return scalar columns), but we handle them for exhaustiveness.
        FalkorValue::Node(node) => Value::String(format!("{node:?}")),
        FalkorValue::Edge(edge) => Value::String(format!("{edge:?}")),
        FalkorValue::Path(path) => Value::String(format!("{path:?}")),
        FalkorValue::Point(point) => Value::String(format!("{point:?}")),
        FalkorValue::Vec32(vec32) => Value::String(format!("{vec32:?}")),
        FalkorValue::Unparseable(s) => Value::String(s),
    }
}

// ---------- Legacy text parsers (retained for unit tests only) ----------
// NOTE: parse_rows is commented out — no longer used in production after the
// typed FalkorDB read path replaced it. Kept for potential future use.
// #[cfg(test)]
// fn parse_rows(raw: &str) -> Vec<Vec<Value>> {
//     parse_graph_snapshot(raw)
//         .and_then(|parsed| parsed.as_array().cloned())
//         .unwrap_or_default()
//         .into_iter()
//         .filter_map(|row| row.as_array().map(|values| values.to_vec()))
//         .collect()
// }

#[cfg(test)]
fn parse_graph_snapshot(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() || raw.trim() == "No results returned." {
        return Some(Value::Array(Vec::new()));
    }
    let mut rows = Vec::new();
    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record_str = if let Some(idx) = trimmed.find('[') {
            &trimmed[idx..]
        } else {
            trimmed
        };
        let record_str = record_str.trim();
        if !record_str.starts_with('[') || !record_str.ends_with(']') {
            tracing::debug!(
                line_idx,
                line_preview = %record_str.chars().take(240).collect::<String>(),
                "Skipping FalkorDB line that is not a row record"
            );
            continue;
        }
        let inner = &record_str[1..record_str.len() - 1];
        let parts = split_top_level(inner, ',');
        let mut values = Vec::new();
        let mut row_ok = true;
        for (part_idx, part) in parts.into_iter().enumerate() {
            match parse_debug_value(part.trim()) {
                Some(value) => values.push(value),
                None => {
                    tracing::debug!(
                        line_idx,
                        part_idx,
                        part_preview = %part.chars().take(240).collect::<String>(),
                        "Skipping FalkorDB row due to unparsable value"
                    );
                    row_ok = false;
                    break;
                }
            }
        }
        if !row_ok {
            continue;
        }
        rows.push(Value::Array(values));
    }
    Some(Value::Array(rows))
}

#[cfg(test)]
fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth_bracket: usize = 0;
    let mut depth_brace: usize = 0;
    let mut depth_paren: usize = 0;
    let mut in_string = false;
    // FalkorDB returns string properties containing JSON with unescaped
    // inner quotes, e.g. `"{"key":"value"}"`. Track brace/bracket depth
    // inside strings so we only exit the string when the embedded JSON is
    // fully closed.
    let mut string_brace_depth: usize = 0;
    let mut string_bracket_depth: usize = 0;
    let mut escape = false;
    for ch in input.chars() {
        if in_string {
            current.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
            } else if ch == '{' {
                string_brace_depth += 1;
            } else if ch == '}' {
                string_brace_depth = string_brace_depth.saturating_sub(1);
            } else if ch == '[' {
                string_bracket_depth += 1;
            } else if ch == ']' {
                string_bracket_depth = string_bracket_depth.saturating_sub(1);
            } else if ch == '"' && string_brace_depth == 0 && string_bracket_depth == 0 {
                in_string = false;
            }
            // else: stay in string — unescaped quote inside embedded JSON
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                string_brace_depth = 0;
                string_bracket_depth = 0;
                current.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                current.push(ch);
            }
            ']' => {
                depth_bracket = depth_bracket.saturating_sub(1);
                current.push(ch);
            }
            '{' => {
                depth_brace += 1;
                current.push(ch);
            }
            '}' => {
                depth_brace = depth_brace.saturating_sub(1);
                current.push(ch);
            }
            '(' => {
                depth_paren += 1;
                current.push(ch);
            }
            ')' => {
                depth_paren = depth_paren.saturating_sub(1);
                current.push(ch);
            }
            _ if ch == delimiter && depth_bracket == 0 && depth_brace == 0 && depth_paren == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

#[cfg(test)]
fn parse_debug_value(input: &str) -> Option<Value> {
    let value = input.trim();
    if value.starts_with("Map(") {
        return parse_debug_map(value);
    }
    if value.starts_with("Array(") {
        return parse_debug_array(value);
    }
    if value.starts_with("String(") {
        return parse_debug_string(value).map(Value::String);
    }
    if value.starts_with("I64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("Integer(") && value.ends_with(')') {
        let inner = &value[8..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("Long(") && value.ends_with(')') {
        let inner = &value[5..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("F64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return serde_json::Number::from_f64(inner.parse::<f64>().ok()?).map(Value::Number);
    }
    if value.starts_with("Bool(") && value.ends_with(')') {
        let inner = &value[5..value.len() - 1];
        return inner.parse::<bool>().ok().map(Value::Bool);
    }
    if value == "Null" || value == "null" {
        return Some(Value::Null);
    }
    if value.starts_with('[') && value.ends_with(']') {
        return parse_bracket_array(value);
    }
    if value.starts_with('"') && value.ends_with('"') {
        return parse_quoted_string_with_json_fallback(value);
    }
    Some(Value::String(value.to_string()))
}

#[cfg(test)]
fn parse_quoted_string_with_json_fallback(value: &str) -> Option<Value> {
    if let Ok(parsed) = serde_json::from_str::<String>(value) {
        return Some(Value::String(parsed));
    }

    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);

    // FalkorDB text snapshots sometimes return a quoted payload whose inner
    // quotes are unescaped, e.g. `"{"k":"v"}"`. Treat the inner segment as
    // JSON and re-serialize so downstream `decode_embedded_json` can parse it.
    if let Ok(parsed_json) = serde_json::from_str::<Value>(inner) {
        return serde_json::to_string(&parsed_json)
            .ok()
            .map(Value::String)
            .or_else(|| Some(Value::String(inner.to_string())));
    }

    Some(Value::String(inner.to_string()))
}

#[cfg(test)]
fn parse_debug_string(value: &str) -> Option<String> {
    if !value.starts_with("String(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[7..value.len() - 1];
    if inner.starts_with('"') && inner.ends_with('"') {
        if let Ok(parsed) = serde_json::from_str::<String>(inner) {
            Some(parsed)
        } else {
            Some(inner.trim_matches('"').to_string())
        }
    } else {
        let wrapped = format!("\"{}\"", inner);
        if let Ok(parsed) = serde_json::from_str::<String>(&wrapped) {
            Some(parsed)
        } else {
            Some(inner.to_string())
        }
    }
}

#[cfg(test)]
fn parse_debug_array(value: &str) -> Option<Value> {
    if !value.starts_with("Array(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[6..value.len() - 1];
    parse_bracket_array(inner)
}

#[cfg(test)]
fn parse_bracket_array(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Some(Value::Array(Vec::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut values = Vec::new();
    for part in parts {
        values.push(parse_debug_value(part.trim())?);
    }
    Some(Value::Array(values))
}

#[cfg(test)]
fn parse_debug_map(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    if !trimmed.starts_with("Map(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed[4..trimmed.len() - 1];
    let inner = inner.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    if inner.trim().is_empty() {
        return Some(Value::Object(serde_json::Map::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut map = serde_json::Map::new();
    for part in parts {
        let mut iter = split_top_level(part.trim(), ':').into_iter();
        let key_raw = iter.next()?.trim().to_string();
        let value_raw = iter.collect::<Vec<String>>().join(":");
        let key = serde_json::from_str::<String>(&key_raw).ok()?;
        let value = parse_debug_value(value_raw.trim())?;
        map.insert(key, value);
    }
    Some(Value::Object(map))
}

fn decode_embedded_json(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        other => other.clone(),
    }
}

fn normalize_message_content(value: &Value) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

fn value_as_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn value_as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "true" | "True" | "TRUE" => Some(true),
            "false" | "False" | "FALSE" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn event_id_counter(event_id: &str) -> u64 {
    event_id
        .strip_prefix("prov-")
        .and_then(|id| id.parse::<u64>().ok())
        .unwrap_or(0)
}

fn has_meaningful_result(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

fn is_empty_object(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}
