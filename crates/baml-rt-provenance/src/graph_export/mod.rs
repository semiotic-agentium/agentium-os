// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Graph export and rendering for provenance subgraphs.
//!
//! This module reads the provenance graph via SurrealDB queries and produces an
//! [`ExportedGraph`] — a portable, renderable representation of nodes and edges.
//! Pure-function renderers then convert `ExportedGraph` into Mermaid, Graphviz
//! DOT, or JSON for frontends, tests, and documentation.
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

use baml_rt_observability::record_provenance_read;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

use crate::{
    error::Result,
    graph_export::activity_outcome::NodeActivityOutcome,
    graph_model::GraphNodeLabel,
    id_semantics::context_entity_id_string,
    spans,
    surreal_store::{SurrealProvenanceStore, check_and_take_zero, map_surreal_error},
    vocabulary::{a2a, context_scope, message_directions, prov, storage_safe},
};

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
    /// Temporal ordering key extracted from `a2a:activity_anchor` (monotonic counter)
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

// ── GraphExporter (SurrealDB) ────────────────────────────────────────────────

/// Reads the SurrealDB provenance graph and produces [`ExportedGraph`] values.
pub struct GraphExporter {
    store: Arc<SurrealProvenanceStore>,
}

impl GraphExporter {
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    /// Export the full subgraph for a given `context_id`.
    pub async fn export_by_context(&self, context_id: &str) -> Result<ExportedGraph> {
        let span = spans::graph_export_by_context(context_id);
        let start = std::time::Instant::now();
        let result = async {
            let graph = self.export_context_core(context_id).await?;
            let allowed: HashSet<String> = std::iter::once(context_id.to_string()).collect();
            Ok(filter_scope_multi(graph, a2a::CONTEXT_ID, &allowed))
        }
        .instrument(span)
        .await;
        let result_label = match &result {
            Ok(_) => "success",
            Err(_) => "error",
        };
        record_provenance_read("export_by_context", result_label, start.elapsed());
        result
    }

    async fn export_context_core(&self, context_id: &str) -> Result<ExportedGraph> {
        tracing::debug!(context_id = %context_id, "export_context_core: START surreal");
        let t0 = std::time::Instant::now();

        let scoped_to = context_scope::SCOPED_TO;
        let ctx_node_id = context_entity_id_string(context_id);

        // Use subqueries to avoid binding Vec<String> which SurrealDB's bind API
        // does not support for IN clauses. The scoped_ids subquery is reused
        // as a building block for nodes and edges.
        let scoped_ids_subquery = format!(
            "(SELECT VALUE from_id FROM prov_edge WHERE to_id = '{ctx_node_id}' AND rel_type = '{scoped_to}')"
        );

        // Fetch all nodes scoped to this context
        let node_query = format!(
            "SELECT node_id, label, props OMIT id FROM prov_node WHERE node_id IN {scoped_ids_subquery}"
        );
        let node_response = self
            .store
            .db()
            .query(&node_query)
            .await
            .map_err(map_surreal_error)?;
        let node_rows: Vec<Value> = check_and_take_zero(node_response, map_surreal_error)?;

        if node_rows.is_empty() {
            tracing::debug!(context_id = %context_id, "no scoped nodes found");
            return Ok(ExportedGraph {
                nodes: vec![],
                edges: vec![],
                scope: ExportScope::Context(context_id.to_string()),
            });
        }

        let scoped_ids: HashSet<String> = node_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(String::from))
            .collect();

        // Fetch all edges where from_id is in the scoped set
        let edge_query = format!(
            "SELECT from_id, from_label, to_id, to_label, rel_type, props OMIT id FROM prov_edge \
             WHERE from_id IN {scoped_ids_subquery} AND rel_type != '{scoped_to}'"
        );
        let edge_response = self
            .store
            .db()
            .query(&edge_query)
            .await
            .map_err(map_surreal_error)?;
        let edge_rows: Vec<Value> = check_and_take_zero(edge_response, map_surreal_error)?;

        // Collect target node IDs not in the scoped set (e.g. AgentRuntimeInstance)
        let extra_ids: HashSet<String> = edge_rows
            .iter()
            .filter_map(|r| r.get("to_id").and_then(Value::as_str).map(String::from))
            .filter(|id| !scoped_ids.contains(id))
            .collect();

        let extra_node_rows: Vec<Value> = if extra_ids.is_empty() {
            Vec::new()
        } else {
            let in_list = extra_ids
                .iter()
                .map(|id| format!("'{id}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT node_id, label, props OMIT id FROM prov_node WHERE node_id IN [{in_list}]"
            );
            self.store.query_sql_rows(&query).await?
        };

        let query_ms = t0.elapsed().as_millis();
        tracing::debug!(context_id = %context_id, query_ms, "export_context_core: DONE surreal, START parse");
        let t1 = std::time::Instant::now();

        let mut graph = parse_surreal_export_result(
            &node_rows,
            &extra_node_rows,
            &edge_rows,
            ExportScope::Context(context_id.to_string()),
        );
        enrich::enrich_derived_properties(&mut graph);
        let parse_ms = t1.elapsed().as_millis();
        tracing::debug!(
            context_id = %context_id,
            query_ms, parse_ms,
            nodes = graph.nodes.len(),
            edges = graph.edges.len(),
            "export_context_core: surreal + parse"
        );

        Ok(graph)
    }

    /// Export the full subgraph for a given `task_id`.
    pub async fn export_by_task(&self, task_id: &str) -> Result<ExportedGraph> {
        let span = spans::graph_export_by_task(task_id);
        let start = std::time::Instant::now();
        let result = async {
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
        .instrument(span)
        .await;
        let result_label = match &result {
            Ok(_) => "success",
            Err(_) => "error",
        };
        record_provenance_read("export_by_task", result_label, start.elapsed());
        result
    }

    /// List all distinct context IDs in the provenance graph.
    pub async fn list_contexts(&self) -> Result<Vec<String>> {
        let start = std::time::Instant::now();
        let result = async {
            let ctx_label = context_scope::LABEL;
            let query = "SELECT node_id OMIT id FROM prov_node WHERE label = $label".to_string();
            let response = self
                .store
                .db()
                .query(&query)
                .bind(("label", ctx_label))
                .await
                .map_err(map_surreal_error)?;
            let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
            let mut ids: Vec<String> = rows
                .iter()
                .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(String::from))
                .filter(|s| !s.is_empty())
                .collect();
            if ids.is_empty() {
                // Fallback: find distinct SCOPED_TO target nodes with Context label.
                let scoped = context_scope::SCOPED_TO;
                let fallback = format!(
                    "SELECT DISTINCT to_id OMIT id FROM prov_edge \
                     WHERE rel_type = '{scoped}' AND to_label = '{ctx_label}'"
                );
                let fb_response = self
                    .store
                    .db()
                    .query(&fallback)
                    .await
                    .map_err(map_surreal_error)?;
                let fb_rows: Vec<Value> = check_and_take_zero(fb_response, map_surreal_error)?;
                ids = fb_rows
                    .iter()
                    .filter_map(|r| r.get("to_id").and_then(Value::as_str).map(String::from))
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            ids.sort();
            ids.dedup();
            Ok(ids)
        }
        .await;
        let result_label = match &result {
            Ok(_) => "success",
            Err(_) => "error",
        };
        record_provenance_read("list_contexts", result_label, start.elapsed());
        result
    }

    /// Resolve the context_id for a task via SCOPED_TO edge traversal.
    async fn task_context_id(&self, task_id: &str) -> Result<Option<String>> {
        let task_node = crate::id_semantics::task_entity_id_string_raw(task_id);
        let scoped = context_scope::SCOPED_TO;
        let query = format!(
            "SELECT to_id OMIT id FROM prov_edge \
             WHERE from_id = $task_node AND rel_type = '{scoped}' LIMIT 1"
        );
        let response = self
            .store
            .db()
            .query(&query)
            .bind(("task_node", task_node))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let ctx_node_id = rows
            .first()
            .and_then(|r| r.get("to_id"))
            .and_then(Value::as_str);
        Ok(ctx_node_id.map(|nid| {
            crate::metamodel::ContextNodeId::new(nid.to_string())
                .to_context_id()
                .into_string()
        }))
    }
}

// ── Convenience wrappers ────────────────────────────────────────────────────

/// Resolve the `context_id` for a task from the graph (single lightweight query).
pub async fn task_context_id(
    store: Arc<SurrealProvenanceStore>,
    task_id: &str,
) -> Result<Option<String>> {
    GraphExporter::new(store).task_context_id(task_id).await
}

/// Convenience wrapper: export the subgraph for a single task.
pub async fn export_graph_for_task(
    store: Arc<SurrealProvenanceStore>,
    task_id: &str,
) -> Result<ExportedGraph> {
    GraphExporter::new(store).export_by_task(task_id).await
}

// ── Parsing (SurrealDB rows) ────────────────────────────────────────────────

/// Parse SurrealDB query results into an [`ExportedGraph`].
fn parse_surreal_export_result(
    node_rows: &[Value],
    extra_node_rows: &[Value],
    edge_rows: &[Value],
    scope: ExportScope,
) -> ExportedGraph {
    let mut nodes_map: HashMap<String, ExportedNode> = HashMap::new();

    for row in node_rows.iter().chain(extra_node_rows.iter()) {
        let node_id = row
            .get("node_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
        let props = surreal_props_to_map(row.get("props"));
        if !node_id.is_empty() {
            upsert_node(&mut nodes_map, node_id, label, &props);
        }
    }

    let mut edges: Vec<ExportedEdge> = Vec::new();
    for row in edge_rows {
        let from_id = row
            .get("from_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let to_id = row
            .get("to_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let rel_type = row
            .get("rel_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let rel_props = surreal_props_to_map(row.get("props"));

        // If target node wasn't fetched yet (rare), insert a stub
        if !to_id.is_empty() && !nodes_map.contains_key(&to_id) {
            let to_label = row
                .get("to_label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            nodes_map.insert(
                to_id.clone(),
                ExportedNode {
                    id: to_id.clone(),
                    label: to_label.to_string(),
                    display_name: to_label.to_string(),
                    properties: HashMap::new(),
                    event_order: None,
                },
            );
        }

        if !from_id.is_empty() && !to_id.is_empty() && !rel_type.is_empty() {
            edges.push(ExportedEdge {
                from: from_id,
                to: to_id,
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

    ExportedGraph {
        nodes,
        edges,
        scope,
    }
}

/// Extract properties from a SurrealDB props object and normalize keys to a2a: form.
fn surreal_props_to_map(props: Option<&Value>) -> HashMap<String, serde_json::Value> {
    let map = match props {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| {
                v.as_object()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            })
            .unwrap_or_default(),
        _ => HashMap::new(),
    };
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
        Some(GraphNodeLabel::AgentStop) => {
            let reason = prop_str("a2a_stop_reason").unwrap_or_default();
            format!("🛑 Stop {reason}")
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
        Some(GraphNodeLabel::SessionStep) => {
            let op = prop_str("op_kind").unwrap_or_default();
            let tool = prop_str("tool_name").unwrap_or_default();
            format!("⚡ {tool} {op}")
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

/// Extract a temporal ordering key from node properties (graph-first).
///
/// Primary: persisted `a2a:event_order` (written at normalization time).
/// Fallback: `a2a:timestamp_ms`, then `a2a:task_state_time` (both stored properties).
fn parse_event_order(props: &HashMap<String, serde_json::Value>) -> Option<u64> {
    if let Some(order) = props.get(a2a::EVENT_ORDER).and_then(|v| v.as_u64()) {
        return Some(order);
    }
    if let Some(ts) = props.get(a2a::TIMESTAMP_MS).and_then(|v| v.as_u64()) {
        return Some(ts);
    }
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
    use crate::vocabulary::semantic_labels;

    #[expect(
        clippy::too_many_arguments,
        reason = "test row builder takes each column explicitly to keep cases readable"
    )]
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
    fn repeated_message_node_prefers_non_empty_role_and_content() {
        let sparse_first = make_row(
            "Message",
            "msg-1",
            serde_json::json!({
                "a2a:role": "",
                "a2a:content": [],
                "a2a:activity_anchor": "prov-1"
            }),
            semantic_labels::WAS_RECEIVED_BY,
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
                "a2a:activity_anchor": "prov-1"
            }),
            semantic_labels::WAS_RECEIVED_BY,
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
        // Content stored as a JSON array (as the provenance store returns it).
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
