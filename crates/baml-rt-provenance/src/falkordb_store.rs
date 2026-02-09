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
use crate::error::Result;
use crate::normalizer::{
    A2aDerivedRelation, DefaultProvNormalizer, NormalizedProv, ProvNormalizer, validate_event,
};
use crate::store::ProvenanceWriter;
use crate::types::{
    Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, QualifiedGeneration, Used,
    WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
};
use crate::vocabulary::{
    a2a, a2a_relation_types, a2a_roles, message_directions, prov, prov_relations, prov_roles,
    semantic_labels,
};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
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

        let mut entity_entries: Vec<(&ProvEntityId, &Entity)> =
            normalized.document.entities().collect();
        entity_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
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
            clauses.push(merge_node(label, id.as_str(), &props));
        }

        let mut activity_entries: Vec<(&ProvActivityId, &Activity)> =
            normalized.document.activities().collect();
        activity_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
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
            clauses.push(merge_node(label, id.as_str(), &props));
        }

        let mut agent_entries: Vec<(&ProvAgentId, &Agent)> = normalized.document.agents().collect();
        agent_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
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
        used_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, used) in used_entries {
            let props = used_props(used);
            let activity_label = label_for_activity(&activity_labels, used.activity.as_str());
            let entity_label = label_for_entity(&entity_labels, used.entity.as_str());
            let rel_type = relation_label("USED", activity_label, entity_label, &props);
            clauses.push(merge_edge(
                activity_label,
                used.activity.as_str(),
                &rel_type,
                entity_label,
                used.entity.as_str(),
                &props,
            ));
        }
        let mut generated_entries: Vec<(&String, &WasGeneratedBy)> =
            normalized.document.was_generated_by().collect();
        generated_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, generated) in generated_entries {
            let props = was_generated_by_props(generated);
            let entity_label = label_for_ref(
                generated.entity.clone(),
                &entity_labels,
                &activity_labels,
                &agent_labels,
            );
            let activity_label = label_for_activity(&activity_labels, generated.activity.as_str());
            let rel_type = relation_label("WAS_GENERATED_BY", entity_label, activity_label, &props);
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
        qualified_gen_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
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
        assoc_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, assoc) in assoc_entries {
            let props = was_associated_with_props(assoc);
            let activity_label = label_for_activity(&activity_labels, assoc.activity.as_str());
            let agent_label = label_for_agent(&agent_labels, assoc.agent.as_str());
            let rel_type =
                relation_label("WAS_ASSOCIATED_WITH", activity_label, agent_label, &props);
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
        derived_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (_, derived) in derived_entries {
            let props = was_derived_from_props(derived);
            let generated_label =
                label_for_entity(&entity_labels, derived.generated_entity.as_str());
            let used_label = label_for_entity(&entity_labels, derived.used_entity.as_str());
            let rel_type = relation_label("WAS_DERIVED_FROM", generated_label, used_label, &props);
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

#[async_trait]
impl ProvenanceWriter for FalkorDbProvenanceWriter {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        validate_event(&event)?;
        let normalized = self.normalizer.normalize(&event)?;
        let query = Self::build_query(&normalized);
        if query.is_empty() {
            return Ok(());
        }
        execute_cypher_query(&query, &self.config.graph, &self.config.connection, false).await?;
        Ok(())
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
            "A2ATaskExecution" => semantic_labels::WAS_SPAWNED_BY,
            "A2AMessageProcessing" => semantic_labels::WAS_RECEIVED_BY,
            "LlmCall" => semantic_labels::WAS_CONSUMED_BY,
            "ToolCall" => semantic_labels::WAS_CONSUMED_BY,
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
            ("A2AMessage", "A2AMessageProcessing") => Some(Self::MessageProcessing),
            ("Artifact", "A2ATaskExecution") => Some(Self::ArtifactTaskExecution),
            ("A2ATask", "A2ATaskExecution") => Some(Self::TaskTaskExecution),
            ("AgentRuntimeInstance", "AgentBoot") => Some(Self::AgentRuntimeInstanceBoot),
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
        "A2A_TASK_CALL" => match to_label {
            "LlmCall" => Some(semantic_labels::WAS_INVOKED_BY),
            "ToolCall" => Some(semantic_labels::WAS_EXECUTED_BY),
            _ => None,
        },
        "A2A_MESSAGE_CALL" => match to_label {
            "LlmCall" => Some(semantic_labels::WAS_INVOKED_BY),
            "ToolCall" => Some(semantic_labels::WAS_EXECUTED_BY),
            _ => None,
        },
        "A2A_TASK_MESSAGE" => match props.get(a2a::DIRECTION).and_then(Value::as_str) {
            Some(message_directions::RECEIVED) => Some(semantic_labels::WAS_SPAWNED_BY),
            Some(message_directions::SENT) => Some(semantic_labels::WAS_EMITTED_BY),
            _ => Some(semantic_labels::WAS_RELATED_TO),
        },
        "A2A_TASK_ARTIFACT" => Some(semantic_labels::WAS_GENERATED_BY),
        "A2A_TASK_STATUS_TRANSITION" => Some(semantic_labels::WAS_TRANSITIONED_TO),
        _ => None,
    };
    let label = semantic.unwrap_or(relation.relation.as_str());
    sanitize_label(label, relation.relation.as_str())
}

fn label_for_entity<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or("ProvEntity")
}

fn label_for_activity<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or("ProvActivity")
}

fn label_for_agent<'a>(labels: &'a HashMap<String, String>, id: &str) -> &'a str {
    labels
        .get(id)
        .map(|value| value.as_str())
        .unwrap_or("ProvAgent")
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
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
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
