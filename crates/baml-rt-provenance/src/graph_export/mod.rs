//! Graph export and rendering for provenance subgraphs.
//!
//! This module reads the GraphQLite provenance graph via Cypher queries and
//! produces an [`ExportedGraph`] — a portable, renderable representation of
//! nodes and edges. Pure-function renderers then convert `ExportedGraph` into
//! Mermaid, Graphviz DOT, or JSON for frontends, tests, and documentation.
//!
//! **No heuristics in projection:** If export/query/render is impossible given
//! the stored graph, the graph construction (write path) is incorrect.

pub mod activity_outcome;
pub mod assertions;
pub mod dot;
pub mod enrich;
pub mod json;
pub mod sequence;
pub mod simplify;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use graphqlite::CypherResult;
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    graph_export::activity_outcome::NodeActivityOutcome,
    graph_model::GraphNodeLabel,
    graphqlite_store::GraphqliteProvenanceStore,
    vocabulary::{a2a, context_scope, message_directions, prov, storage_safe},
};

fn single_param(key: &str, value: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut params = serde_json::Map::new();
    params.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    params
}

// ── Core types ──────────────────────────────────────────────────────────────

/// A node in the exported provenance graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportedNode {
    /// Stable identity key (e.g. `"task:task-1"`, `"llm_call:prov-6"`).
    pub id: String,
    /// Graph label (e.g. `"LlmCall"`, `"ToolCall"`, `"Message"`).
    pub label: String,
    /// Human-readable display name derived from properties.
    pub display_name: String,
    /// Selected properties for display (tool_name, model, role, etc.).
    pub properties: HashMap<String, serde_json::Value>,
    /// Temporal ordering key extracted from `a2a:event_id` (monotonic counter)
    /// or `a2a:timestamp_ms` as a fallback. `None` for nodes without temporal
    /// metadata (e.g. `AgentRuntimeInstance`).
    pub event_order: Option<u64>,
}

/// An edge in the exported provenance graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportedEdge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
    /// Relationship type (e.g. `"WAS_EXECUTED_BY"`, `"WAS_USED_BY"`).
    pub relation: String,
    /// Edge properties (prov:role, a2a:direction, etc.).
    pub properties: HashMap<String, serde_json::Value>,
}

/// A complete exported subgraph, scoped to a context_id or task_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportedGraph {
    /// All nodes in the subgraph.
    pub nodes: Vec<ExportedNode>,
    /// All edges in the subgraph.
    pub edges: Vec<ExportedEdge>,
    /// The query scope (context_id, task_id, or "full").
    pub scope: ExportScope,
}

/// Scope discriminant for an exported subgraph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExportScope {
    /// Scoped to a specific `a2a:context_id`.
    Context(String),
    /// Scoped to a specific `a2a:task_id`.
    Task(String),
    /// Full graph, no scope filter.
    Full,
}

// ── GraphExporter (GraphQLite) ───────────────────────────────────────────────

/// Reads the GraphQLite provenance graph and produces [`ExportedGraph`] values.
pub struct GraphExporter {
    store: Arc<GraphqliteProvenanceStore>,
}

impl GraphExporter {
    pub fn new(store: Arc<GraphqliteProvenanceStore>) -> Self {
        Self { store }
    }

    /// Export the full subgraph for a given `context_id`.
    ///
    /// Traverses SCOPED_TO edges. AgentRuntimeInstance has a2a:archive_path at write.
    /// [`filter_scope`] keeps boot-chain nodes via [`context_scope::SCOPE_EXEMPT_LABELS`].
    #[tracing::instrument(skip(self), fields(context_id))]
    pub async fn export_by_context(&self, context_id: &str) -> Result<ExportedGraph> {
        let graph = self.export_context_core(context_id).await?;
        let allowed: HashSet<String> = std::iter::once(context_id.to_string()).collect();
        Ok(filter_scope_multi(graph, a2a::CONTEXT_ID, &allowed))
    }

    async fn export_context_core(&self, context_id: &str) -> Result<ExportedGraph> {
        tracing::debug!(context_id = %context_id, "export_context_core: START cypher");
        // Traverse via SCOPED_TO edges (indexed by Context.id). No property filters.
        // Only (a) must be scoped; (b) is reached via the relation (e.g. MessageProcessing
        // -[WAS_EXECUTED_BY]-> AgentRuntimeInstance). AgentRuntimeInstance has no context.
        let ctx_escaped = context_id.replace('\'', "''");
        let query = format!(
            "MATCH (ctx:{ctx_label} {{id: '{ctx_escaped}'}})-[:{scoped_to}]->(a), (a)-[r]->(b) \
             RETURN a.id AS src_id, labels(a)[0] AS src_label, properties(a) AS src_props, \
                    type(r) AS rel_type, properties(r) AS rel_props, \
                    b.id AS tgt_id, labels(b)[0] AS tgt_label, properties(b) AS tgt_props",
            ctx_label = context_scope::LABEL,
            scoped_to = context_scope::SCOPED_TO,
        );
        let params = serde_json::Map::new();
        let t0 = std::time::Instant::now();
        let result = self.store.run_cypher_read(&query, &params).await?;
        let cypher_ms = t0.elapsed().as_millis();
        tracing::debug!(context_id = %context_id, cypher_ms, "export_context_core: DONE cypher, START parse");
        let t1 = std::time::Instant::now();
        let mut graph =
            parse_graphqlite_export_result(&result, ExportScope::Context(context_id.to_string()))?;
        enrich::enrich_derived_properties(&mut graph);
        let parse_ms = t1.elapsed().as_millis();
        tracing::debug!(
            context_id = %context_id,
            cypher_ms,
            parse_ms,
            nodes = graph.nodes.len(),
            edges = graph.edges.len(),
            "export_context_core: cypher + parse"
        );

        Ok(graph)
    }

    /// Export the full subgraph for a given `task_id`.
    ///
    /// Resolves the task's context_id (from TaskExecution etc.), then runs the same
    /// SCOPED_TO traversal as export_by_context so the initial user message (which
    /// has context_id but no task_id) is included.
    #[tracing::instrument(skip(self), fields(task_id))]
    pub async fn export_by_task(&self, task_id: &str) -> Result<ExportedGraph> {
        let context_id = self.task_context_id(task_id).await?;
        let graph = if let Some(ctx_id) = context_id {
            let mut g = self.export_context_core(&ctx_id).await?;
            g.scope = ExportScope::Task(task_id.to_string());
            g
        } else {
            ExportedGraph {
                nodes: vec![],
                edges: vec![],
                scope: ExportScope::Task(task_id.to_string()),
            }
        };
        Ok(filter_scope(graph, a2a::TASK_ID, task_id))
    }

    /// List all distinct context IDs in the provenance graph.
    ///
    /// Tries Context nodes first; falls back to scanning nodes with a2a_context_id.
    pub async fn list_contexts(&self) -> Result<Vec<String>> {
        let ctx_label = context_scope::LABEL;
        let query = format!("MATCH (ctx:{ctx_label}) RETURN ctx.id AS ctx_id");
        let params = serde_json::Map::new();
        let result = self.store.run_cypher_read(&query, &params).await?;
        let mut ids: Vec<String> = result
            .iter()
            .filter_map(|row| row_get_string_any(row, &["ctx_id"]))
            .filter(|s| !s.is_empty())
            .collect();
        if ids.is_empty() {
            // Fallback: nodes may have a2a_context_id without a Context node (legacy or sparse writes).
            let fallback = "MATCH (n) WHERE n.a2a_context_id IS NOT NULL RETURN DISTINCT n.a2a_context_id AS ctx_id";
            let result = self.store.run_cypher_read(fallback, &params).await?;
            ids = result
                .iter()
                .filter_map(|row| row_get_string_any(row, &["ctx_id"]))
                .filter(|s| !s.is_empty())
                .collect();
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Get context_id for a task from any node with a2a_task_id and a2a_context_id (e.g. TaskExecution).
    async fn task_context_id(&self, task_id: &str) -> Result<Option<String>> {
        let query = "MATCH (t) WHERE t.a2a_task_id = $task_id AND t.a2a_context_id IS NOT NULL \
             RETURN t.a2a_context_id AS ctx_id LIMIT 1";
        let params = single_param("task_id", task_id);
        let result = self.store.run_cypher_read(query, &params).await?;
        let ctx_id = result
            .iter()
            .next()
            .and_then(|row| row_get_string_any(row, &["ctx_id"]));
        Ok(ctx_id)
    }
}

// ── Parsing (GraphQLite rows) ───────────────────────────────────────────────

/// Parse GraphQLite Cypher result into an [`ExportedGraph`].
///
/// Expects columns: src_id, src_label, src_props, rel_type, rel_props, tgt_id, tgt_label, tgt_props.
/// Properties may be returned as JSON object or JSON string; keys are normalized from
/// storage_safe (a2a_*) to vocabulary (a2a:*) for display and filtering.
fn parse_graphqlite_export_result(
    result: &CypherResult,
    scope: ExportScope,
) -> Result<ExportedGraph> {
    let mut nodes_map: HashMap<String, ExportedNode> = HashMap::new();
    let mut edges: Vec<ExportedEdge> = Vec::new();

    for row in result.iter() {
        let src_id = row_get_string_any(row, &["src_id", "a.id"]).unwrap_or_default();
        let src_label = row_get_string_any(row, &["src_label", "labels(a)[0]"]).unwrap_or_default();
        let rel_type = row_get_string_any(row, &["rel_type", "type(r)"]).unwrap_or_default();
        let rel_props = row_to_properties(
            row,
            &["rel_props", "properties(r)", "toString(properties(r))"],
        );
        let tgt_id = row_get_string_any(row, &["tgt_id", "b.id"]).unwrap_or_default();
        let tgt_label = row_get_string_any(row, &["tgt_label", "labels(b)[0]"]).unwrap_or_default();

        if !src_id.is_empty() {
            let needs_enrichment = nodes_map
                .get(src_id.as_str())
                .is_none_or(|existing| node_needs_enrichment(existing, &src_label));
            if needs_enrichment {
                let src_props = row_to_properties(
                    row,
                    &["src_props", "properties(a)", "toString(properties(a))"],
                );
                upsert_node(&mut nodes_map, &src_id, &src_label, &src_props);
            }
        }
        if !tgt_id.is_empty() {
            let needs_enrichment = nodes_map
                .get(tgt_id.as_str())
                .is_none_or(|existing| node_needs_enrichment(existing, &tgt_label));
            if needs_enrichment {
                let tgt_props = row_to_properties(
                    row,
                    &["tgt_props", "properties(b)", "toString(properties(b))"],
                );
                upsert_node(&mut nodes_map, &tgt_id, &tgt_label, &tgt_props);
            }
        }
        let relation = rel_type.trim().to_string();
        if !src_id.is_empty() && !tgt_id.is_empty() && !relation.is_empty() {
            edges.push(ExportedEdge {
                from: src_id,
                to: tgt_id,
                relation,
                properties: rel_props,
            });
        }
    }

    let order_of: HashMap<String, Option<u64>> = nodes_map
        .iter()
        .map(|(id, node)| (id.clone(), node.event_order))
        .collect();

    let mut nodes: Vec<ExportedNode> = nodes_map.into_values().collect();
    nodes.sort_by(|a, b| cmp_event_order(a.event_order, &a.id, b.event_order, &b.id));

    edges.sort_by(|a, b| {
        let a_from_ord = order_of.get(a.from.as_str()).copied().flatten();
        let b_from_ord = order_of.get(b.from.as_str()).copied().flatten();
        let a_to_ord = order_of.get(a.to.as_str()).copied().flatten();
        let b_to_ord = order_of.get(b.to.as_str()).copied().flatten();
        cmp_event_order(a_from_ord, &a.from, b_from_ord, &b.from)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| cmp_event_order(a_to_ord, &a.to, b_to_ord, &b.to))
    });
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.relation == b.relation);

    Ok(ExportedGraph {
        nodes,
        edges,
        scope,
    })
}

fn row_get_string_any(row: &graphqlite::Row, cols: &[&str]) -> Option<String> {
    cols.iter().find_map(|col| row.get::<String>(col).ok())
}

/// Read a properties column from a GraphQLite row (JSON string) and normalize keys to a2a: form.
fn row_to_properties(row: &graphqlite::Row, cols: &[&str]) -> HashMap<String, serde_json::Value> {
    let as_json = cols.iter().find_map(|col| {
        row.get_value(col).map(|v| match v {
            graphqlite::Value::Null => serde_json::Value::Null,
            graphqlite::Value::Bool(v) => serde_json::Value::Bool(*v),
            graphqlite::Value::Integer(v) => serde_json::Value::Number((*v).into()),
            graphqlite::Value::Float(v) => serde_json::Number::from_f64(*v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            graphqlite::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
                .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
            graphqlite::Value::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        graphqlite::Value::Null => serde_json::Value::Null,
                        graphqlite::Value::Bool(v) => serde_json::Value::Bool(*v),
                        graphqlite::Value::Integer(v) => serde_json::Value::Number((*v).into()),
                        graphqlite::Value::Float(v) => serde_json::Number::from_f64(*v)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                        graphqlite::Value::String(s) => {
                            serde_json::from_str::<serde_json::Value>(s)
                                .unwrap_or_else(|_| serde_json::Value::String(s.clone()))
                        }
                        graphqlite::Value::Array(_) | graphqlite::Value::Object(_) => {
                            serde_json::to_value(item).unwrap_or(serde_json::Value::Null)
                        }
                    })
                    .collect(),
            ),
            graphqlite::Value::Object(_) => {
                serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
            }
        })
    });
    let map: HashMap<String, serde_json::Value> = as_json
        .and_then(|v| {
            v.as_object().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<HashMap<String, serde_json::Value>>()
            })
        })
        .unwrap_or_else(|| {
            let s = row_get_string_any(row, cols).unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .and_then(|v| {
                    v.as_object().map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<HashMap<String, serde_json::Value>>()
                    })
                })
                .unwrap_or_default()
        });
    normalize_property_keys(map)
}

/// Convert storage_safe keys (a2a_*) to vocabulary keys (a2a:*) for display and filter_scope.
fn normalize_property_keys(
    map: HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    map.into_iter()
        .map(|(k, v)| {
            let normalized = if k.starts_with("a2a_") {
                k.replacen("a2a_", "a2a:", 1)
            } else if k.starts_with("prov_") {
                k.replacen("prov_", "prov:", 1)
            } else {
                k
            };
            (normalized, v)
        })
        .collect()
}

/// Merge/update node entry for a repeated node id.
///
/// Some graph backends may return multiple rows for the same node where one row
/// has sparse properties (e.g. missing role/content) and another has richer
/// properties. We merge by preferring non-empty incoming values over empty
/// existing ones to avoid freezing sparse state.
fn upsert_node(
    nodes_map: &mut HashMap<String, ExportedNode>,
    node_id: &str,
    node_label: &str,
    node_props: &HashMap<String, serde_json::Value>,
) {
    let incoming_event_order = parse_event_order(node_props);
    if let Some(existing) = nodes_map.get_mut(node_id) {
        if existing.label.is_empty() && !node_label.is_empty() {
            existing.label = node_label.to_string();
        }
        existing.event_order = match (existing.event_order, incoming_event_order) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        for (k, incoming) in node_props {
            let should_replace = existing
                .properties
                .get(k)
                .is_none_or(property_value_is_empty)
                && !property_value_is_empty(incoming);
            if should_replace {
                existing.properties.insert(k.clone(), incoming.clone());
            }
        }
        existing.display_name = derive_display_name(&existing.label, &existing.properties);
        return;
    }

    nodes_map.insert(
        node_id.to_string(),
        ExportedNode {
            display_name: derive_display_name(node_label, node_props),
            id: node_id.to_string(),
            label: node_label.to_string(),
            event_order: incoming_event_order,
            properties: node_props.clone(),
        },
    );
}

fn property_value_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn node_needs_enrichment(existing: &ExportedNode, incoming_label: &str) -> bool {
    if existing.label.is_empty() && !incoming_label.is_empty() {
        return true;
    }
    if existing.event_order.is_none() {
        return true;
    }
    if GraphNodeLabel::parse(&existing.label).or_else(|| GraphNodeLabel::parse(incoming_label))
        == Some(GraphNodeLabel::Message)
    {
        let missing_role = existing
            .properties
            .get(a2a::ROLE)
            .is_none_or(property_value_is_empty);
        let missing_content = existing
            .properties
            .get(a2a::CONTENT)
            .is_none_or(property_value_is_empty);
        let missing_event_id = existing
            .properties
            .get(a2a::EVENT_ID)
            .is_none_or(property_value_is_empty);
        return missing_role || missing_content || missing_event_id;
    }
    false
}

// ── Scope post-filtering ────────────────────────────────────────────────────

/// Post-filter an exported graph to remove nodes that belong to a different
/// scope (context or task).
///
/// The only reason for this filter is the boot chain: AgentBoot and AgentArchive
/// have no context_id at all. They are exempt (see [`context_scope::SCOPE_EXEMPT_LABELS`])
/// so they remain in the graph for agent attribution.
///
/// A node is **kept** if:
/// - Its label is in [`context_scope::SCOPE_EXEMPT_LABELS`], OR
/// - It has no `property_key` property at all, OR
/// - Its `property_key` value matches `expected_value`.
///
/// Edges referencing any removed node are also dropped.
fn filter_scope(graph: ExportedGraph, property_key: &str, expected_value: &str) -> ExportedGraph {
    filter_scope_multi(
        graph,
        property_key,
        &HashSet::from([expected_value.to_string()]),
    )
}

/// Like [`filter_scope`] but allows multiple values (e.g. primary + initiator context).
fn filter_scope_multi(
    graph: ExportedGraph,
    property_key: &str,
    allowed_values: &HashSet<String>,
) -> ExportedGraph {
    let removed: std::collections::HashSet<String> = graph
        .nodes
        .iter()
        .filter(|n| {
            if context_scope::SCOPE_EXEMPT_LABELS.contains(&n.label.as_str()) {
                return false;
            }
            n.properties
                .get(property_key)
                .and_then(|v| v.as_str())
                .is_some_and(|v| !allowed_values.contains(v))
        })
        .map(|n| n.id.clone())
        .collect();

    if removed.is_empty() {
        return graph;
    }

    let nodes: Vec<ExportedNode> = graph
        .nodes
        .into_iter()
        .filter(|n| !removed.contains(&n.id))
        .collect();

    let edges: Vec<ExportedEdge> = graph
        .edges
        .into_iter()
        .filter(|e| !removed.contains(&e.from) && !removed.contains(&e.to))
        .collect();

    ExportedGraph {
        nodes,
        edges,
        scope: graph.scope,
    }
}

// ── Display name derivation ─────────────────────────────────────────────────

/// Default maximum length for content previews in display names.
const DEFAULT_CONTENT_PREVIEW_LEN: usize = 60;

/// Default maximum length for args summaries in display names.
const DEFAULT_ARGS_SUMMARY_LEN: usize = 80;

/// Derive a human-readable display name from a node's label and properties.
///
/// Handles graph backend data shapes: JSON arrays for content, raw enum
/// role variants, metadata objects with `phase`, etc.
fn derive_display_name(label: &str, props: &HashMap<String, serde_json::Value>) -> String {
    // Extract a string property. Handles `Value::String` directly, and also
    // stringifies numbers/booleans so callers can use it for mixed-type fields.
    let prop_str = |key: &str| -> Option<String> {
        props.get(key).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
    };

    match GraphNodeLabel::parse(label) {
        Some(GraphNodeLabel::Intent) => {
            let intent_id = prop_str(a2a::INTENT_ID).unwrap_or_default();
            let label = prop_str(storage_safe::PROV_LABEL)
                .or_else(|| prop_str(prov::LABEL))
                .unwrap_or_default();
            format!("🎯 Intent {intent_id} {label}")
        }
        Some(GraphNodeLabel::Plan) => {
            let plan_id = prop_str(a2a::PLAN_ID).unwrap_or_default();
            let intent_id = prop_str(a2a::INTENT_ID).unwrap_or_default();
            format!("🗺️ Plan {plan_id} (intent {intent_id})")
        }
        Some(GraphNodeLabel::PlanStep) => {
            let step_id = prop_str(a2a::STEP_ID).unwrap_or_default();
            let status = prop_str(a2a::STATUS).unwrap_or_default();
            let label = prop_str(storage_safe::PROV_LABEL)
                .or_else(|| prop_str(prov::LABEL))
                .unwrap_or_default();
            format!("🧩 Step {step_id} [{status}] {label}")
        }
        Some(GraphNodeLabel::Message) => {
            let role = normalize_role(&prop_str(a2a::ROLE).unwrap_or_default());
            let direction = prop_str(a2a::DIRECTION).unwrap_or_default();
            let icon = if direction == message_directions::SENT {
                "📤"
            } else {
                "📩"
            };
            let content =
                extract_content_preview(props.get(a2a::CONTENT), DEFAULT_CONTENT_PREVIEW_LEN);
            format!("{icon} {role}: {content}")
        }
        Some(GraphNodeLabel::MessageProcessing) => {
            let msg_id = prop_str(a2a::MESSAGE_ID).unwrap_or_default();
            format!("🔄 MsgProc {msg_id}")
        }
        Some(GraphNodeLabel::LlmCall) => {
            let model = prop_str(a2a::MODEL).unwrap_or_default();
            let func = prop_str(a2a::FUNCTION_NAME).unwrap_or_default();
            let mut name = format!("🤖 LLM {model} ({func})");
            if let Some(duration) = prop_str(a2a::DURATION_MS) {
                name.push_str(&format!(" {duration}ms"));
            }
            if let Some(outcome) = NodeActivityOutcome::from_props(props) {
                name.push_str(outcome.display_suffix());
            }
            name
        }
        Some(GraphNodeLabel::ToolCall) => {
            let tool = strip_tool_prefix(&prop_str(a2a::TOOL_NAME).unwrap_or_default());
            let phase = extract_metadata_field(props.get(a2a::METADATA), "phase");
            let mut name = match phase {
                Some(p) => format!("🔧 {tool} ({p})"),
                None => format!("🔧 {tool}"),
            };
            if let Some(outcome) = NodeActivityOutcome::from_props(props) {
                name.push_str(outcome.display_suffix());
            }
            name
        }
        Some(GraphNodeLabel::ToolArgs) => {
            let summary = summarize_args(props.get(a2a::ARGS), DEFAULT_ARGS_SUMMARY_LEN);
            format!("📋 Args {summary}")
        }
        Some(GraphNodeLabel::Task) => {
            let tid = prop_str(a2a::TASK_ID).unwrap_or_default();
            format!("📌 Task {tid}")
        }
        Some(GraphNodeLabel::TaskExecution) => {
            let tid = prop_str(a2a::TASK_ID).unwrap_or_default();
            format!("⚙️ TaskExec {tid}")
        }
        Some(GraphNodeLabel::TaskState) => {
            let state = prop_str(a2a::TASK_STATE).unwrap_or_default();
            format!("📊 State {state}")
        }
        Some(GraphNodeLabel::AgentRuntimeInstance) => {
            let agent_type = prop_str(a2a::AGENT_TYPE).unwrap_or_default();
            format!("🖥️ Agent {agent_type}")
        }
        Some(GraphNodeLabel::AgentBoot) => {
            let agent_type = prop_str(a2a::AGENT_TYPE).unwrap_or_default();
            format!("🚀 Boot {agent_type}")
        }
        Some(GraphNodeLabel::AgentArchive) => {
            let path = prop_str(a2a::ARCHIVE_PATH).unwrap_or_default();
            format!("📦 Archive {path}")
        }
        Some(GraphNodeLabel::Artifact) => {
            let art_type = prop_str(a2a::ARTIFACT_TYPE).unwrap_or_default();
            format!("📄 Artifact {art_type}")
        }
        Some(GraphNodeLabel::LlmPrompt) => "💬 Prompt".to_string(),
        Some(GraphNodeLabel::PromptRejected) => {
            let reason = prop_str(a2a::REASON).unwrap_or_default();
            format!("⚠️ Rejected {reason}")
        }
        Some(GraphNodeLabel::FailureClassificationActivity) => {
            let evidence = prop_str(a2a::FAILURE_EVIDENCE).unwrap_or_default();
            format!("🧭 FailureClassify {evidence}")
        }
        Some(GraphNodeLabel::FailureClassification) => {
            let class = prop_str(a2a::FAILURE_CLASS).unwrap_or_default();
            let evidence = prop_str(a2a::FAILURE_EVIDENCE).unwrap_or_default();
            format!("📉 Failure {class} [{evidence}]")
        }
        None => label.to_string(),
    }
}

/// Normalize a role string: strip `ROLE_` prefix and lowercase.
///
/// Examples: `"ROLE_USER"` → `"user"`, `"assistant"` → `"assistant"`.
fn normalize_role(raw: &str) -> String {
    let stripped = raw.strip_prefix("ROLE_").unwrap_or(raw);
    stripped.to_lowercase()
}

/// Extract a content preview from `a2a:content`.
///
/// The value may be:
/// - A `String` → use directly.
/// - An `Array` → take the first element's string representation.
/// - An `Object` or JSON-encoded string wrapping an array → parse and extract.
fn extract_content_preview(value: Option<&serde_json::Value>, max_len: usize) -> String {
    let Some(val) = value else {
        return String::new();
    };

    let text = match val {
        serde_json::Value::String(s) => {
            // Could be a plain string or a JSON-encoded array like `["hello"]`.
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(s)
            {
                first_array_element_text(&arr)
            } else {
                s.clone()
            }
        }
        serde_json::Value::Array(arr) => first_array_element_text(arr),
        other => other.to_string(),
    };

    truncate_str(&text, max_len)
}

/// Extract the text of the first element in a JSON array.
fn first_array_element_text(arr: &[serde_json::Value]) -> String {
    match arr.first() {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Strip common tool name prefixes for brevity.
///
/// `"support/clickupNavigate"` → `"clickupNavigate"`, `"system/internal_a2a"` → `"internal_a2a"`.
fn strip_tool_prefix(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("support/") {
        return rest.to_string();
    }
    if let Some(rest) = name.strip_prefix("system/") {
        return rest.to_string();
    }
    name.to_string()
}

/// Extract a field from a JSON metadata value.
///
/// `a2a:metadata` may be a `Value::Object` directly, or a `Value::String`
/// containing JSON.
fn extract_metadata_field(value: Option<&serde_json::Value>, field: &str) -> Option<String> {
    match value? {
        serde_json::Value::Object(map) => map.get(field).and_then(|v| v.as_str()).map(String::from),
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .and_then(|v| v.get(field).and_then(|f| f.as_str()).map(String::from)),
        _ => None,
    }
}

/// Produce a generic summary of `a2a:args` for ToolArgs display.
///
/// Enumerates top-level key=value pairs where the value is a scalar (string,
/// number, bool). Skips nulls and nested objects/arrays. Truncates to
/// `max_len`. Returns `"(empty)"` for `{}`.
fn summarize_args(value: Option<&serde_json::Value>, max_len: usize) -> String {
    let Some(val) = value else {
        return "(empty)".to_string();
    };

    let obj = match val {
        serde_json::Value::Object(map) => map.clone(),
        serde_json::Value::String(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return truncate_str(s, max_len),
        },
        _ => return "(empty)".to_string(),
    };

    if obj.is_empty() {
        return "(empty)".to_string();
    }

    let pairs: Vec<String> = obj
        .iter()
        .filter_map(|(k, v)| {
            match v {
                serde_json::Value::String(s) => Some(format!("{k}={s}")),
                serde_json::Value::Number(n) => Some(format!("{k}={n}")),
                serde_json::Value::Bool(b) => Some(format!("{k}={b}")),
                // Skip nulls, arrays, and objects — not useful for a summary.
                _ => None,
            }
        })
        .collect();

    if pairs.is_empty() {
        return "(empty)".to_string();
    }

    truncate_str(&pairs.join(" "), max_len)
}

/// Truncate a string to `max_len` characters, appending `…` if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

// ── Private helpers ─────────────────────────────────────────────────────────

/// Compare two nodes/edges by event_order, falling back to id for stability.
///
/// `None` sorts after `Some` so nodes without temporal data sink to the end.
fn cmp_event_order(
    a_order: Option<u64>,
    a_id: &str,
    b_order: Option<u64>,
    b_id: &str,
) -> std::cmp::Ordering {
    match (a_order, b_order) {
        (Some(a), Some(b)) => a.cmp(&b).then_with(|| a_id.cmp(b_id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a_id.cmp(b_id),
    }
}

/// Extract a temporal ordering key from node properties.
///
/// Primary: parse the monotonic counter from `a2a:event_id` (`"prov-42"` → 42).
/// Fallback: use `a2a:timestamp_ms`, then `a2a:task_state_time` for TaskState nodes.
fn parse_event_order(props: &HashMap<String, serde_json::Value>) -> Option<u64> {
    // Try a2a:event_id first (format: "prov-{counter}").
    if let Some(event_id) = props.get(a2a::EVENT_ID).and_then(|v| v.as_str())
        && let Some(counter_str) = event_id.strip_prefix("prov-")
        && let Ok(counter) = counter_str.parse::<u64>()
    {
        return Some(counter);
    }
    // Fallback to a2a:timestamp_ms.
    if let Some(ts) = props.get(a2a::TIMESTAMP_MS).and_then(|v| v.as_u64()) {
        return Some(ts);
    }
    // Fallback for TaskState nodes: a2a:task_state_time.
    props.get(a2a::TASK_STATE_TIME).and_then(|v| v.as_u64())
}

/// Extract a `String` from a JSON value (returns empty string for non-strings).
fn value_as_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => {
            let s = other.to_string();
            // Strip surrounding quotes if serde serialized a string
            s.trim_matches('"').to_string()
        }
    }
}

/// Extract a flat property map from a JSON value (object or JSON string).
/// Used by tests that build ExportedGraph from raw JSON.
fn extract_properties(v: &serde_json::Value) -> HashMap<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .and_then(|v| {
                v.as_object()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            })
            .unwrap_or_default(),
        _ => HashMap::new(),
    }
}

/// Build ExportedGraph from JSON rows (each row: [src_label, src_id, src_props, rel_type, rel_props, tgt_label, tgt_id, tgt_props]).
/// Used by tests and by any caller that has row data as JSON.
pub fn build_graph_from_json_rows(
    rows: &[Vec<serde_json::Value>],
    scope: ExportScope,
) -> Result<ExportedGraph> {
    const EXPORT_COLUMN_COUNT: usize = 8;
    let mut nodes_map: HashMap<String, ExportedNode> = HashMap::new();
    let mut edges: Vec<ExportedEdge> = Vec::new();

    for row in rows {
        let cols = match row.as_slice() {
            c if c.len() >= EXPORT_COLUMN_COUNT => c,
            _ => continue,
        };
        let src_label = value_as_str(&cols[0]);
        let src_id = value_as_str(&cols[1]);
        let src_props = extract_properties(&cols[2]);
        let rel_type = value_as_str(&cols[3]);
        let rel_props = extract_properties(&cols[4]);
        let tgt_label = value_as_str(&cols[5]);
        let tgt_id = value_as_str(&cols[6]);
        let tgt_props = extract_properties(&cols[7]);

        if !src_id.is_empty() {
            upsert_node(&mut nodes_map, &src_id, &src_label, &src_props);
        }
        if !tgt_id.is_empty() {
            upsert_node(&mut nodes_map, &tgt_id, &tgt_label, &tgt_props);
        }
        if !src_id.is_empty() && !tgt_id.is_empty() && !rel_type.is_empty() {
            edges.push(ExportedEdge {
                from: src_id,
                to: tgt_id,
                relation: rel_type,
                properties: rel_props,
            });
        }
    }

    let order_of: HashMap<String, Option<u64>> = nodes_map
        .iter()
        .map(|(id, node)| (id.clone(), node.event_order))
        .collect();
    let mut nodes: Vec<ExportedNode> = nodes_map.into_values().collect();
    nodes.sort_by(|a, b| cmp_event_order(a.event_order, &a.id, b.event_order, &b.id));
    edges.sort_by(|a, b| {
        let a_from_ord = order_of.get(a.from.as_str()).copied().flatten();
        let b_from_ord = order_of.get(b.from.as_str()).copied().flatten();
        let a_to_ord = order_of.get(a.to.as_str()).copied().flatten();
        let b_to_ord = order_of.get(b.to.as_str()).copied().flatten();
        cmp_event_order(a_from_ord, &a.from, b_from_ord, &b.from)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| cmp_event_order(a_to_ord, &a.to, b_to_ord, &b.to))
    });
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.relation == b.relation);

    Ok(ExportedGraph {
        nodes,
        edges,
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn make_row(
        src_label: &str,
        src_id: &str,
        src_props: serde_json::Value,
        rel_type: &str,
        rel_props: serde_json::Value,
        tgt_label: &str,
        tgt_id: &str,
        tgt_props: serde_json::Value,
    ) -> Vec<serde_json::Value> {
        vec![
            serde_json::Value::String(src_label.to_string()),
            serde_json::Value::String(src_id.to_string()),
            src_props,
            serde_json::Value::String(rel_type.to_string()),
            rel_props,
            serde_json::Value::String(tgt_label.to_string()),
            serde_json::Value::String(tgt_id.to_string()),
            tgt_props,
        ]
    }

    #[test]
    fn parse_export_result_empty_input() {
        let graph = build_graph_from_json_rows(&[], ExportScope::Full).expect("should parse empty");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn parse_export_result_single_edge() {
        let row = make_row(
            "ToolCall",
            "prov-1",
            serde_json::json!({"a2a:tool_name": "support/clickup"}),
            "WAS_USED_BY",
            serde_json::json!({}),
            "ToolArgs",
            "prov-2",
            serde_json::json!({"a2a:args": "{\"action\":\"CreateTask\"}"}),
        );
        let graph = build_graph_from_json_rows(&[row], ExportScope::Context("ctx-1".into()))
            .expect("should parse");

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);

        let tool_node = graph.nodes.iter().find(|n| n.label == "ToolCall").unwrap();
        assert_eq!(tool_node.id, "prov-1");
        assert!(
            tool_node.display_name.contains("clickup"),
            "display_name: {}",
            tool_node.display_name
        );
        assert!(
            !tool_node.display_name.contains("support/"),
            "display_name: {}",
            tool_node.display_name
        );

        let args_node = graph.nodes.iter().find(|n| n.label == "ToolArgs").unwrap();
        assert!(
            args_node.display_name.contains("action=CreateTask"),
            "display_name: {}",
            args_node.display_name
        );

        let edge = &graph.edges[0];
        assert_eq!(edge.from, "prov-1");
        assert_eq!(edge.to, "prov-2");
        assert_eq!(edge.relation, "WAS_USED_BY");
    }

    #[test]
    fn parse_export_result_deduplicates_nodes() {
        let row1 = make_row(
            "Message",
            "msg-1",
            serde_json::json!({"a2a:role": "user"}),
            "WAS_RECEIVED_BY",
            serde_json::json!({}),
            "A2AMessageProcessing",
            "mp-1",
            serde_json::json!({}),
        );
        let row2 = make_row(
            "A2AMessageProcessing",
            "mp-1",
            serde_json::json!({}),
            "WAS_EXECUTED_BY",
            serde_json::json!({}),
            "ToolCall",
            "tc-1",
            serde_json::json!({"a2a:tool_name": "support/clickup"}),
        );
        let graph =
            build_graph_from_json_rows(&[row1, row2], ExportScope::Full).expect("should parse");
        assert_eq!(graph.nodes.len(), 3, "msg-1, mp-1, tc-1 should be unique");
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn derive_display_name_covers_all_labels() {
        let cases = vec![
            (
                "Message",
                vec![(a2a::ROLE, "user"), (a2a::CONTENT, "Hello world")],
            ),
            ("A2AMessageProcessing", vec![(a2a::MESSAGE_ID, "msg-1")]),
            (
                "LlmCall",
                vec![(a2a::MODEL, "deepseek/v3"), (a2a::FUNCTION_NAME, "Chat")],
            ),
            ("ToolCall", vec![(a2a::TOOL_NAME, "support/clickup")]),
            ("ToolArgs", vec![(a2a::ARGS, r#"{"action":"CreateTask"}"#)]),
            ("A2ATask", vec![(a2a::TASK_ID, "task-1")]),
            ("A2ATaskExecution", vec![(a2a::TASK_ID, "task-1")]),
            ("A2ATaskState", vec![(a2a::TASK_STATE, "completed")]),
            ("AgentRuntimeInstance", vec![(a2a::AGENT_TYPE, "tony")]),
            ("AgentBoot", vec![(a2a::AGENT_TYPE, "tony")]),
            (
                "AgentArchive",
                vec![(a2a::ARCHIVE_PATH, "/tmp/archive.zip")],
            ),
            ("Artifact", vec![(a2a::ARTIFACT_TYPE, "file")]),
            ("LlmPrompt", vec![]),
        ];
        for (label, props_list) in cases {
            let props: HashMap<String, serde_json::Value> = props_list
                .into_iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect();
            let name = derive_display_name(label, &props);
            assert!(
                !name.is_empty(),
                "display name for {label} should not be empty"
            );
        }
    }

    #[test]
    fn parse_export_result_no_results_message() {
        let graph = build_graph_from_json_rows(&[], ExportScope::Full).expect("empty rows");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn edges_are_sorted_and_deduped() {
        let row = make_row(
            "Message",
            "msg-1",
            serde_json::json!({}),
            "WAS_RECEIVED_BY",
            serde_json::json!({}),
            "A2AMessageProcessing",
            "mp-1",
            serde_json::json!({}),
        );
        let graph = build_graph_from_json_rows(&[row.clone(), row], ExportScope::Full)
            .expect("should parse");
        assert_eq!(graph.edges.len(), 1, "duplicate edges should be deduped");
    }

    #[test]
    fn repeated_message_node_prefers_non_empty_role_and_content() {
        let sparse_first = make_row(
            "Message",
            "msg-1",
            serde_json::json!({
                "a2a:role": "",
                "a2a:content": [],
                "a2a:event_id": "prov-1"
            }),
            "WAS_RECEIVED_BY",
            serde_json::json!({}),
            "A2AMessageProcessing",
            "mp-1",
            serde_json::json!({}),
        );
        let richer_second = make_row(
            "Message",
            "msg-1",
            serde_json::json!({
                "a2a:role": "ROLE_USER",
                "a2a:content": ["hello world"],
                "a2a:event_id": "prov-1"
            }),
            "WAS_RECEIVED_BY",
            serde_json::json!({}),
            "A2AMessageProcessing",
            "mp-2",
            serde_json::json!({}),
        );

        let graph = build_graph_from_json_rows(&[sparse_first, richer_second], ExportScope::Full)
            .expect("should parse");
        let msg = graph
            .nodes
            .iter()
            .find(|n| n.id == "msg-1")
            .expect("message node exists");
        assert_eq!(
            msg.properties.get(a2a::ROLE),
            Some(&serde_json::Value::String("ROLE_USER".to_string()))
        );
        assert_eq!(
            msg.properties.get(a2a::CONTENT),
            Some(&serde_json::json!(["hello world"]))
        );
    }

    // ── Display name enrichment tests (§15 fixes) ───────────────────────

    #[test]
    fn message_content_from_json_array() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("ROLE_USER".to_string()),
        );
        // Content stored as a JSON array (as GraphQLite returns it).
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(
                "please create a task, make it Test11".to_string(),
            )]),
        );
        let name = derive_display_name("Message", &props);
        assert!(
            name.contains("user:"),
            "role should be normalized to 'user': {name}"
        );
        assert!(
            !name.contains("ROLE_"),
            "ROLE_ prefix should be stripped: {name}"
        );
        assert!(
            name.contains("please create a task"),
            "content preview should be extracted from array: {name}"
        );
    }

    #[test]
    fn message_content_from_json_encoded_array_string() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("assistant".to_string()),
        );
        // Content stored as a JSON-encoded string wrapping an array.
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::String(r#"["I will create that task for you"]"#.to_string()),
        );
        let name = derive_display_name("Message", &props);
        assert!(
            name.contains("I will create that task"),
            "should parse JSON-encoded array string: {name}"
        );
    }

    /// Delegated messages from coordinator contain JSON plan; extract "objective" for display.

    #[test]
    fn role_normalization() {
        assert_eq!(normalize_role("ROLE_USER"), "user");
        assert_eq!(normalize_role("ROLE_ASSISTANT"), "assistant");
        assert_eq!(normalize_role("assistant"), "assistant");
        assert_eq!(normalize_role("user"), "user");
        assert_eq!(normalize_role("ROLE_CUSTOM_AGENT"), "custom_agent");
    }

    #[test]
    fn tool_call_shows_phase_and_strips_prefix() {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String("support/clickupNavigate".to_string()),
        );
        props.insert(
            a2a::METADATA.to_string(),
            serde_json::json!({"phase": "send", "correlation_id": "corr-1"}),
        );
        let name = derive_display_name("ToolCall", &props);
        assert_eq!(name, "🔧 clickupNavigate (send)");
    }

    #[test]
    fn tool_call_without_phase() {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String("memory/tony".to_string()),
        );
        let name = derive_display_name("ToolCall", &props);
        assert_eq!(name, "🔧 memory/tony");
    }

    #[test]
    fn tool_call_metadata_as_json_string() {
        let mut props = HashMap::new();
        props.insert(
            a2a::TOOL_NAME.to_string(),
            serde_json::Value::String("support/clickupMutate".to_string()),
        );
        // Metadata stored as a JSON-encoded string (common in graph backends).
        props.insert(
            a2a::METADATA.to_string(),
            serde_json::Value::String(
                r#"{"phase":"finish","correlation_id":"corr-2"}"#.to_string(),
            ),
        );
        let name = derive_display_name("ToolCall", &props);
        assert_eq!(name, "🔧 clickupMutate (finish)");
    }

    #[test]
    fn tool_args_summarizes_scalars() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ARGS.to_string(),
            serde_json::json!({"action": "ListTeams", "team_id": null, "verbose": true}),
        );
        let name = derive_display_name("ToolArgs", &props);
        // Should contain action=ListTeams and verbose=true, skip null team_id.
        assert!(
            name.contains("action=ListTeams"),
            "should include scalar pairs: {name}"
        );
        assert!(
            name.contains("verbose=true"),
            "should include bool pairs: {name}"
        );
        assert!(!name.contains("team_id"), "should skip null values: {name}");
    }

    #[test]
    fn tool_args_empty_object() {
        let mut props = HashMap::new();
        props.insert(a2a::ARGS.to_string(), serde_json::json!({}));
        let name = derive_display_name("ToolArgs", &props);
        assert!(
            name.contains("(empty)"),
            "empty args should show (empty): {name}"
        );
    }

    #[test]
    fn tool_args_from_json_string() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ARGS.to_string(),
            serde_json::Value::String(r#"{"action":"CreateTask","name":"Demo"}"#.to_string()),
        );
        let name = derive_display_name("ToolArgs", &props);
        assert!(
            name.contains("action=CreateTask"),
            "should parse JSON string args: {name}"
        );
    }

    #[test]
    fn llm_call_shows_duration_and_success() {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("deepseek/v3".to_string()),
        );
        props.insert(
            a2a::FUNCTION_NAME.to_string(),
            serde_json::Value::String("AgentChat".to_string()),
        );
        props.insert(
            a2a::DURATION_MS.to_string(),
            serde_json::Value::Number(5232.into()),
        );
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::Value::String("Success".to_string()),
        );
        let name = derive_display_name("LlmCall", &props);
        assert!(name.contains("5232ms"), "should show duration: {name}");
        assert!(name.contains('✅'), "should show success indicator: {name}");
    }

    #[test]
    fn llm_call_without_completion_info() {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("unknown".to_string()),
        );
        props.insert(
            a2a::FUNCTION_NAME.to_string(),
            serde_json::Value::String("Chat".to_string()),
        );
        let name = derive_display_name("LlmCall", &props);
        assert_eq!(name, "🤖 LLM unknown (Chat)");
        assert!(!name.contains("ms"), "no duration should be appended");
    }

    #[test]
    fn truncate_str_works() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 6), "hello…");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn strip_tool_prefix_strips_support() {
        assert_eq!(strip_tool_prefix("support/clickup"), "clickup");
        assert_eq!(strip_tool_prefix("memory/tony"), "memory/tony");
        assert_eq!(strip_tool_prefix("plain_tool"), "plain_tool");
        assert_eq!(strip_tool_prefix("system/internal_a2a"), "internal_a2a");
    }

    // ── Direction-aware display name tests ────────────────────────────────

    #[test]
    fn sent_message_shows_outbox_icon() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("ROLE_AGENT".to_string()),
        );
        props.insert(
            a2a::DIRECTION.to_string(),
            serde_json::Value::String("sent".to_string()),
        );
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::String("Done! Created task Test11.".to_string()),
        );
        let name = derive_display_name("Message", &props);
        assert!(
            name.starts_with('\u{1f4e4}'),
            "sent message should use \u{1f4e4} icon: {name}"
        );
        assert!(
            name.contains("agent:"),
            "ROLE_AGENT should normalize to 'agent': {name}"
        );
        assert!(
            name.contains("Done! Created task Test11."),
            "content should be present: {name}"
        );
    }

    #[test]
    fn received_message_shows_inbox_icon() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("ROLE_USER".to_string()),
        );
        props.insert(
            a2a::DIRECTION.to_string(),
            serde_json::Value::String("received".to_string()),
        );
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::String("create a task".to_string()),
        );
        let name = derive_display_name("Message", &props);
        assert!(
            name.starts_with('\u{1f4e9}'),
            "received message should use \u{1f4e9} icon: {name}"
        );
    }

    #[test]
    fn message_without_direction_defaults_to_inbox_icon() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("user".to_string()),
        );
        props.insert(
            a2a::CONTENT.to_string(),
            serde_json::Value::String("hi".to_string()),
        );
        let name = derive_display_name("Message", &props);
        assert!(
            name.starts_with('\u{1f4e9}'),
            "no direction should default to \u{1f4e9}: {name}"
        );
    }
}
