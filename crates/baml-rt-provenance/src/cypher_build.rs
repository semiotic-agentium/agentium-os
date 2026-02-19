//! Cypher query building from normalized PROV documents.
//!
//! Produces parameterised Cypher statements for GraphQLite. Use
//! [build_queries_with_key_style_params] and execute each with `cypher_builder().params().run()`.
//! All values are bound via params—do not inline or escape; read the extension’s tests for behaviour.
//!
//! ## Purpose
//!
//! Translates a normalized PROV document (entities, activities, agents, relations) into a
//! **sequence of parameterised Cypher statements** that can be executed one-by-one by a graph
//! backend (GraphQLite). Each statement is independent: one MERGE or MATCH/CREATE
//! per execution. No batching; the caller runs each [CypherStatement] in order.
//!
//! ## Key transforms
//!
//! | Input                    | Output (per item)                                      |
//! |--------------------------|--------------------------------------------------------|
//! | Entity                   | One node MERGE: `MERGE (n:Label {id: …}) [ON CREATE SET …]` |
//! | Activity                 | One node MERGE (same shape)                           |
//! | Agent                    | One node MERGE (same shape)                           |
//! | Used / WasGeneratedBy / …| **Backtick**: one combined MERGE path. **StorageSafeUnderscore**: three statements (see [merge_edge_statements]) |
//!
//! ## KeyStyle and backends
//!
//! - **[KeyStyle::StorageSafeUnderscore]** (GraphQLite): property names like `a2a_context_id`;
//!   node identity in MERGE uses an **inline Cypher string literal** (not `$id`) so the extension
//!   matches correctly; edges are split into MERGE-from, MERGE-to, MATCH+CREATE edge.
//! - **[KeyStyle::Backtick]**: property names like `` `a2a:context_id` ``; single MERGE path per
//!   edge; params used for identity where needed.
//!
//! ## Invariants
//!
//! - **One statement, one execution**: ∀ statement s ∈ output: s is run separately; order matters
//!   (nodes before edges that reference them).
//! - **Node identity**: ∀ node MERGE: identity key is unique per document (e.g. entity/activity id).
//! - **No inline values in StorageSafeUnderscore except node id**: All other values are bound via
//!   params so the driver handles escaping; node id is inlined as a Cypher literal for GraphQLite.
//!
//! ## Usage
//!
//! Use [build_queries_with_key_style_params] and execute each statement with
//! `cypher_builder(&stmt.query).params(&stmt.params).run()`. Read queries live in
//! [crate::graph_model::ConversationReadModel]; this module only builds **write** statements.

use crate::graph_model::GraphNodeLabel;
use crate::normalizer::{A2aDerivedRelation, NormalizedProv};
use crate::types::{
    Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, QualifiedGeneration, Used,
    WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
};
use crate::vocabulary::{
    a2a, a2a_relation_types, a2a_relations, a2a_roles, base_types, graph, message_directions, prov,
    prov_relations, prov_roles, semantic_labels, storage_safe,
};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Collects placeholder names and values for a parameterized Cypher query.
/// Use [build_query_with_key_style_params]; do not escape values manually.
struct ParamCollector {
    next: usize,
    params: Map<String, Value>,
}

impl ParamCollector {
    fn new() -> Self {
        Self {
            next: 0,
            params: Map::new(),
        }
    }

    /// Add a param; complex values (array/object) are serialized to JSON strings so the
    /// driver receives only primitives. GraphQLite binds params to SQL (no Cypher string
    /// substitution), so string values may contain any character; see executor_helpers.c
    /// bind_params_from_json and colliery-io/graphqlite docs.
    fn add(&mut self, value: Value) -> String {
        let key = format!("p{}", self.next);
        self.next += 1;
        self.add_named(&key, value)
    }

    /// Add a named param (e.g. "id") so the query uses $id. Use for MERGE map identity to match
    /// extension expectations (test_executor_params: CREATE with $name, $age; tool_index: $id).
    fn add_named(&mut self, key: &str, value: Value) -> String {
        let bound = match &value {
            Value::Array(_) | Value::Object(_) => {
                Value::String(serde_json::to_string(&value).expect("serialize param"))
            }
            _ => value.clone(),
        };
        self.params.insert(key.to_string(), bound);
        format!("${key}")
    }

    fn into_value(self) -> Value {
        Value::Object(self.params)
    }
}

/// Escape a string for use inside a Cypher single-quoted string literal (double single quotes).
fn escape_cypher_string(s: &str) -> String {
    s.replace('\'', "''")
}

/// Key style for property names in generated Cypher.
/// Use [KeyStyle::StorageSafeUnderscore] for GraphQLite; use [KeyStyle::Backtick] for backtick-style backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStyle {
    /// Use backticks for unsafe identifiers (e.g. `a2a:context_id`).
    Backtick,
    /// Storage-safe keys (e.g. a2a_context_id). GraphQLite; read queries use the same constant names.
    StorageSafeUnderscore,
}

/// A single parameterised Cypher statement: one query string and one params object.
///
/// **Invariant:** Each statement is executed separately by the caller; order matters (nodes
/// before edges that reference them). Run with `cypher_builder(&stmt.query).params(&stmt.params).run()`.
#[derive(Clone, Debug)]
pub struct CypherStatement {
    pub query: String,
    pub params: Value,
}

/// Alias for a single statement (query, params). Prefer [CypherStatement] for new code.
pub type TypedCypherStatement = (String, Value);

/// Builds a sequence of parameterised Cypher statements from a normalized PROV document.
///
/// One statement per node (entity, activity, agent) and one or more per edge (see
/// [merge_edge_statements]). Execute in order: nodes before edges that reference them.
pub fn build_queries_with_key_style_params(
    normalized: &NormalizedProv,
    key_style: KeyStyle,
) -> Vec<CypherStatement> {
    let mut queries = Vec::new();

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
        let mut collector = ParamCollector::new();
        let clause = merge_node_param(label, id.as_str(), &props, key_style, &mut collector);
        queries.push(CypherStatement {
            query: clause,
            params: collector.into_value(),
        });
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
        let mut collector = ParamCollector::new();
        let clause = merge_node_param(label, id.as_str(), &props, key_style, &mut collector);
        queries.push(CypherStatement {
            query: clause,
            params: collector.into_value(),
        });
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
        let mut collector = ParamCollector::new();
        let clause = merge_node_param(label, id.as_str(), &props, key_style, &mut collector);
        queries.push(CypherStatement {
            query: clause,
            params: collector.into_value(),
        });
    }

    let mut used_entries: Vec<(&String, &Used)> = normalized.document.used().collect();
    used_entries.sort_by_key(|(a, _)| *a);
    for (_, used) in used_entries {
        let props = used_props(used);
        let activity_label = label_for_activity(&activity_labels, used.activity.as_str());
        let entity_label = label_for_entity(&entity_labels, used.entity.as_str());
        let rel_type = relation_label(prov_relations::USED, activity_label, entity_label, &props);
        queries.extend(merge_edge_statements(
            activity_label,
            used.activity.as_str(),
            &rel_type,
            entity_label,
            used.entity.as_str(),
            &props,
            key_style,
        ));
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
        queries.extend(merge_edge_statements(
            entity_label,
            generated.entity.id(),
            &rel_type,
            activity_label,
            generated.activity.as_str(),
            &props,
            key_style,
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
        queries.extend(merge_edge_statements(
            entity_label,
            generation.entity.id(),
            &rel_type,
            activity_label,
            generation.activity.as_str(),
            &props,
            key_style,
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
        queries.extend(merge_edge_statements(
            activity_label,
            assoc.activity.as_str(),
            &rel_type,
            agent_label,
            assoc.agent.as_str(),
            &props,
            key_style,
        ));
    }
    let mut derived_entries: Vec<(&String, &WasDerivedFrom)> =
        normalized.document.was_derived_from().collect();
    derived_entries.sort_by_key(|(a, _)| *a);
    for (_, derived) in derived_entries {
        let props = was_derived_from_props(derived);
        let generated_label = label_for_entity(&entity_labels, derived.generated_entity.as_str());
        let used_label = label_for_entity(&entity_labels, derived.used_entity.as_str());
        let rel_type = relation_label(
            prov_relations::WAS_DERIVED_FROM,
            generated_label,
            used_label,
            &props,
        );
        queries.extend(merge_edge_statements(
            generated_label,
            derived.generated_entity.as_str(),
            &rel_type,
            used_label,
            derived.used_entity.as_str(),
            &props,
            key_style,
        ));
    }

    for relation in &normalized.derived_relations {
        let props = relation_props(relation);
        let from_label = label_for_ref(
            relation.from.clone(),
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let to_label = label_for_ref(
            relation.to.clone(),
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let rel_type = derived_relation_label(relation, from_label, to_label, &props);
        queries.extend(merge_edge_statements(
            from_label,
            relation.from.id(),
            &rel_type,
            to_label,
            relation.to.id(),
            &props,
            key_style,
        ));
    }

    queries
}

/// Emit placeholder and bind value via params. No inline escaping—extension binds as agtype.
fn cypher_value_param(value: &Value, collector: &mut ParamCollector) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => collector.add(value.clone()),
    }
}

fn cypher_map_param(
    map: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    if map.is_empty() {
        return "{}".to_string();
    }
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by_key(|(a, _)| *a);
    let parts: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "{}: {}",
                cypher_key(key, key_style),
                cypher_value_param(value, collector)
            )
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// Build SET n.key = $pN, ... for node properties (excluding id). Used so MERGE map has only one
/// placeholder; GraphQLite's parser accepts MERGE (n:Label {id: $id}) SET n.x = $p0, ...
fn node_set_clause_param(
    props: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    let mut entries: Vec<(&String, &Value)> =
        props.iter().filter(|(k, _)| *k != graph::NODE_ID).collect();
    entries.sort_by_key(|(a, _)| *a);
    let parts: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "n.{} = {}",
                cypher_key(key, key_style),
                cypher_value_param(value, collector)
            )
        })
        .collect();
    parts.join(", ")
}

/// Builds a single node MERGE clause (one statement per node).
///
/// **StorageSafeUnderscore:** Inlines node id as a Cypher string literal so GraphQLite matches
/// correctly; other props in ON CREATE SET. **Backtick:** Uses params for identity and SET n += {…}.
fn merge_node_param(
    label: &str,
    id: &str,
    props: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    let id_value = Value::String(id.to_string());
    match key_style {
        KeyStyle::StorageSafeUnderscore => {
            let mut set_props = props.clone();
            if let Some(Value::Array(arr)) = set_props.get(a2a::CONTENT) {
                let s: String = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .join("\n");
                set_props.insert(a2a::CONTENT.to_string(), Value::String(s));
            }
            // Inline id as Cypher string literal so MERGE pattern matches correctly (GraphQLite may not bind $id in MERGE).
            let id_literal = format!("'{}'", escape_cypher_string(id));
            let id_key = cypher_key(graph::NODE_ID, key_style);
            let set_clause = node_set_clause_param(&set_props, key_style, collector);
            if set_clause.is_empty() {
                format!("MERGE (n:{label} {{{id_key}: {id_literal}}})")
            } else {
                format!(
                    "MERGE (n:{label} {{{id_key}: {id_literal}}}) ON CREATE SET {set_clause}",
                    set_clause = set_clause
                )
            }
        }
        KeyStyle::Backtick => format!(
            "MERGE (n:{label} {{{id_key}: {id_ph}}}) SET n += {props}",
            id_key = graph::NODE_ID,
            id_ph = cypher_value_param(&id_value, collector),
            props = cypher_map_param(props, key_style, collector)
        ),
    }
}

fn edge_set_clause_pairs_param(
    map: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    if map.is_empty() {
        return "r += {}".to_string();
    }
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by_key(|(a, _)| *a);
    let parts: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "r.{} = {}",
                cypher_key(key, key_style),
                cypher_value_param(value, collector)
            )
        })
        .collect();
    parts.join(", ")
}

/// Build {key: $pN, ...} for relationship properties in CREATE (a)-[r:REL {...}]->(b).
fn edge_props_map_param(
    map: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    if map.is_empty() {
        return "{}".to_string();
    }
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by_key(|(a, _)| *a);
    let parts: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "{}: {}",
                cypher_key(key, key_style),
                cypher_value_param(value, collector)
            )
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// Builds a single combined MERGE path for an edge (from-node, to-node, relationship).
/// Used only for [KeyStyle::Backtick]. For GraphQLite use [merge_edge_statements].
#[allow(clippy::too_many_arguments)]
fn merge_edge_param(
    from_label: &str,
    from_id: &str,
    rel_type: &str,
    to_label: &str,
    to_id: &str,
    props: &HashMap<String, Value>,
    key_style: KeyStyle,
    collector: &mut ParamCollector,
) -> String {
    let from_value = Value::String(from_id.to_string());
    let to_value = Value::String(to_id.to_string());
    let from_ph = cypher_value_param(&from_value, collector);
    let to_ph = cypher_value_param(&to_value, collector);
    let id_key = cypher_key(graph::NODE_ID, key_style);
    let base = format!(
        "MERGE (a:{from_label} {{{id_key}: {from_ph}}}) MERGE (b:{to_label} {{{id_key}: {to_ph}}}) MERGE (a)-[r:{rel_type}]->(b)",
    );
    if props.is_empty() {
        base
    } else {
        let set_clause = edge_set_clause_pairs_param(props, key_style, collector);
        format!("{base} SET {set_clause}")
    }
}

/// Builds one or more Cypher statements for a single edge (from-node → relationship → to-node).
///
/// **Purpose:** GraphQLite’s parser does not accept a single combined MERGE path for nodes and
/// relationship (e.g. `MERGE (a)-[r:REL]->(b)` with param-bound identity). Splitting into
/// separate statements ensures each clause is valid and executed in dependency order.
///
/// **Backtick:** Returns a single statement: one combined MERGE for both nodes and
/// the relationship (params used for identity).
///
/// **StorageSafeUnderscore (GraphQLite):** Returns exactly three statements, in order:
///
/// 1. **MERGE from-node** – ensure source node exists
///    `MERGE (a:FromLabel {id: $id})`
/// 2. **MERGE to-node** – ensure target node exists
///    `MERGE (b:ToLabel {id: $id})`
/// 3. **MATCH + CREATE edge** – link them (no MERGE on relationship; SET after CREATE leaves `r` unbound, so props go in CREATE map)
///    `MATCH (a:...), (b:...) CREATE (a)-[r:RelType {…}]->(b)`
///
/// **Invariant:** Execution order must be 1 → 2 → 3 so nodes exist before the MATCH.
fn merge_edge_statements(
    from_label: &str,
    from_id: &str,
    rel_type: &str,
    to_label: &str,
    to_id: &str,
    props: &HashMap<String, Value>,
    key_style: KeyStyle,
) -> Vec<CypherStatement> {
    match key_style {
        KeyStyle::Backtick => {
            let mut collector = ParamCollector::new();
            let clause = merge_edge_param(
                from_label,
                from_id,
                rel_type,
                to_label,
                to_id,
                props,
                key_style,
                &mut collector,
            );
            vec![CypherStatement {
                query: clause,
                params: collector.into_value(),
            }]
        }
        KeyStyle::StorageSafeUnderscore => {
            let id_key = cypher_key(graph::NODE_ID, key_style);
            let from_id_value = Value::String(from_id.to_string());
            let to_id_value = Value::String(to_id.to_string());

            let mut stmts = Vec::with_capacity(3);

            let mut c1 = ParamCollector::new();
            let ph1 = c1.add_named("id", from_id_value.clone());
            stmts.push(CypherStatement {
                query: format!("MERGE (a:{from_label} {{{id_key}: {ph1}}})"),
                params: c1.into_value(),
            });

            let mut c2 = ParamCollector::new();
            let ph2 = c2.add_named("id", to_id_value.clone());
            stmts.push(CypherStatement {
                query: format!("MERGE (b:{to_label} {{{id_key}: {ph2}}})"),
                params: c2.into_value(),
            });

            let mut c3 = ParamCollector::new();
            let from_ph = c3.add_named("from_id", from_id_value);
            let to_ph = c3.add_named("to_id", to_id_value);
            let path = if props.is_empty() {
                format!(
                    "MATCH (a:{from_label} {{{id_key}: {from_ph}}}), (b:{to_label} {{{id_key}: {to_ph}}}) CREATE (a)-[r:{rel_type}]->(b)"
                )
            } else {
                let rel_map = edge_props_map_param(props, key_style, &mut c3);
                format!(
                    "MATCH (a:{from_label} {{{id_key}: {from_ph}}}), (b:{to_label} {{{id_key}: {to_ph}}}) CREATE (a)-[r:{rel_type} {rel_map}]->(b)"
                )
            };
            stmts.push(CypherStatement {
                query: path,
                params: c3.into_value(),
            });

            stmts
        }
    }
}

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

/// Exposed for store validation (mapping_nodes_for_event).
pub(crate) fn label_from_prov_type(prov_type: Option<&str>, fallback: &str) -> String {
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
        crate::types::ProvNodeRef::Agent(id) => label_for_agent(agent_labels, id.as_str()),
    }
}

fn insert_id_props(props: &mut HashMap<String, Value>, id: &str) {
    props.insert(graph::NODE_ID.to_string(), Value::String(id.to_string()));
}

/// Map vocabulary key to storage-safe constant (GraphQLite). No programmatic alteration; unknown keys pass through.
fn storage_safe_key(key: &str) -> String {
    let s: &'static str = match key {
        graph::NODE_ID => graph::NODE_ID,
        prov::TYPE => storage_safe::PROV_TYPE,
        prov::ROLE => storage_safe::PROV_ROLE,
        prov::BASE_TYPE => storage_safe::PROV_BASE_TYPE,
        prov::TIME => storage_safe::PROV_TIME,
        prov::ACTIVITY => storage_safe::PROV_ACTIVITY,
        prov::START_TIME => storage_safe::PROV_START_TIME,
        prov::END_TIME => storage_safe::PROV_END_TIME,
        a2a::AGENT_ID => storage_safe::A2A_AGENT_ID,
        a2a::AGENT_TYPE => storage_safe::A2A_AGENT_TYPE,
        a2a::AGENT_VERSION => storage_safe::A2A_AGENT_VERSION,
        a2a::TASK_ID => storage_safe::A2A_TASK_ID,
        a2a::TASK_STATE => storage_safe::A2A_TASK_STATE,
        a2a::TASK_STATE_TIME => storage_safe::A2A_TASK_STATE_TIME,
        a2a::OLD_STATUS => storage_safe::A2A_OLD_STATUS,
        a2a::IS_PREVIOUS => storage_safe::A2A_IS_PREVIOUS,
        a2a::MESSAGE_ID => storage_safe::A2A_MESSAGE_ID,
        a2a::ROLE => storage_safe::A2A_ROLE,
        a2a::CONTENT => storage_safe::A2A_CONTENT,
        a2a::DIRECTION => storage_safe::A2A_DIRECTION,
        a2a::METADATA => storage_safe::A2A_METADATA,
        a2a::EVENT_ID => storage_safe::A2A_EVENT_ID,
        a2a::RELATION => storage_safe::A2A_RELATION,
        a2a::FROM => storage_safe::A2A_FROM,
        a2a::TO => storage_safe::A2A_TO,
        a2a::CLIENT => storage_safe::A2A_CLIENT,
        a2a::MODEL => storage_safe::A2A_MODEL,
        a2a::FUNCTION_NAME => storage_safe::A2A_FUNCTION_NAME,
        a2a::PROMPT => storage_safe::A2A_PROMPT,
        a2a::USAGE_PROMPT_TOKENS => storage_safe::A2A_USAGE_PROMPT_TOKENS,
        a2a::USAGE_COMPLETION_TOKENS => storage_safe::A2A_USAGE_COMPLETION_TOKENS,
        a2a::USAGE_TOTAL_TOKENS => storage_safe::A2A_USAGE_TOTAL_TOKENS,
        a2a::DURATION_MS => storage_safe::A2A_DURATION_MS,
        a2a::SUCCESS => storage_safe::A2A_SUCCESS,
        a2a::TOOL_NAME => storage_safe::A2A_TOOL_NAME,
        a2a::ARGS => storage_safe::A2A_ARGS,
        a2a::ARCHIVE_PATH => storage_safe::A2A_ARCHIVE_PATH,
        a2a::ARTIFACT_ID => storage_safe::A2A_ARTIFACT_ID,
        a2a::ARTIFACT_TYPE => storage_safe::A2A_ARTIFACT_TYPE,
        a2a::CONTEXT_ID => storage_safe::A2A_CONTEXT_ID,
        a2a::TIMESTAMP_MS => storage_safe::A2A_TIMESTAMP_MS,
        _ => return key.to_string(),
    };
    s.to_string()
}

fn cypher_key(key: &str, key_style: KeyStyle) -> String {
    let out = match key_style {
        KeyStyle::StorageSafeUnderscore => storage_safe_key(key),
        KeyStyle::Backtick => key.to_string(),
    };
    if is_safe_identifier(&out) {
        out
    } else {
        format!("`{}`", out.replace('`', "``"))
    }
}

fn is_safe_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
