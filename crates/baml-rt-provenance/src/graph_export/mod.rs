//! Graph export and rendering for provenance subgraphs.
//!
//! This module reads the FalkorDB provenance graph via Cypher queries and
//! produces an [`ExportedGraph`] — a portable, renderable representation of
//! nodes and edges.  Pure-function renderers then convert `ExportedGraph` into
//! Mermaid, Graphviz DOT, or JSON for frontends, tests, and documentation.

pub mod assertions;
pub mod dot;
pub mod json;
pub mod sequence;
pub mod simplify;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use text_to_cypher::core::execute_cypher_query;

use crate::cypher_parse::{decode_embedded_json, parse_graph_snapshot};
use crate::error::ProvenanceError;
use crate::falkordb_store::FalkorDbProvenanceConfig;
use crate::graph_model::GraphNodeLabel;
use crate::vocabulary::{a2a, message_directions};

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

// ── GraphExporter ───────────────────────────────────────────────────────────

/// Reads the FalkorDB provenance graph and produces [`ExportedGraph`] values.
pub struct GraphExporter {
    config: FalkorDbProvenanceConfig,
}

impl GraphExporter {
    pub fn new(config: FalkorDbProvenanceConfig) -> Self {
        Self { config }
    }

    /// Export the full subgraph for a given `context_id`.
    ///
    /// The Cypher query uses `OR` to be inclusive (agents may have a different
    /// boot context). A Rust-level post-filter then removes nodes whose
    /// `a2a:context_id` doesn't match, exempting agent nodes which
    /// legitimately cross context boundaries.
    #[tracing::instrument(skip(self), fields(context_id))]
    pub async fn export_by_context(&self, context_id: &str) -> crate::error::Result<ExportedGraph> {
        let query = format!(
            r#"MATCH (n)-[r]->(m)
               WHERE n.`a2a:context_id` = "{context_id}"
                  OR m.`a2a:context_id` = "{context_id}"
               RETURN labels(n)[0] AS src_label, n.name AS src_id, properties(n) AS src_props,
                      type(r) AS rel_type, properties(r) AS rel_props,
                      labels(m)[0] AS tgt_label, m.name AS tgt_id, properties(m) AS tgt_props
               ORDER BY n.`a2a:event_id`, type(r), m.`a2a:event_id`"#
        );
        let raw = execute_cypher_query(&query, &self.config.graph, &self.config.connection, true)
            .await
            .map_err(ProvenanceError::Storage)?;

        let graph = parse_export_result(&raw, ExportScope::Context(context_id.to_string()))?;
        Ok(filter_scope(graph, a2a::CONTEXT_ID, context_id))
    }

    /// Export the full subgraph for a given `task_id`.
    ///
    /// Same inclusive-then-filter strategy as [`export_by_context`].
    #[tracing::instrument(skip(self), fields(task_id))]
    pub async fn export_by_task(&self, task_id: &str) -> crate::error::Result<ExportedGraph> {
        let query = format!(
            r#"MATCH (n)-[r]->(m)
               WHERE n.`a2a:task_id` = "{task_id}"
                  OR m.`a2a:task_id` = "{task_id}"
               RETURN labels(n)[0] AS src_label, n.name AS src_id, properties(n) AS src_props,
                      type(r) AS rel_type, properties(r) AS rel_props,
                      labels(m)[0] AS tgt_label, m.name AS tgt_id, properties(m) AS tgt_props
               ORDER BY n.`a2a:event_id`, type(r), m.`a2a:event_id`"#
        );
        let raw = execute_cypher_query(&query, &self.config.graph, &self.config.connection, true)
            .await
            .map_err(ProvenanceError::Storage)?;

        let graph = parse_export_result(&raw, ExportScope::Task(task_id.to_string()))?;
        Ok(filter_scope(graph, a2a::TASK_ID, task_id))
    }

    /// Export the entire graph (use with caution — for dev/debug only).
    #[tracing::instrument(skip(self))]
    pub async fn export_full(&self) -> crate::error::Result<ExportedGraph> {
        let query = r#"MATCH (n)-[r]->(m)
                       RETURN labels(n)[0] AS src_label, n.name AS src_id, properties(n) AS src_props,
                              type(r) AS rel_type, properties(r) AS rel_props,
                              labels(m)[0] AS tgt_label, m.name AS tgt_id, properties(m) AS tgt_props
                       ORDER BY n.name, type(r), m.name"#;
        let raw = execute_cypher_query(query, &self.config.graph, &self.config.connection, true)
            .await
            .map_err(ProvenanceError::Storage)?;

        parse_export_result(&raw, ExportScope::Full)
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Expected column count returned by the graph export Cypher query.
///
/// Columns: src_label, src_id, src_props, rel_type, rel_props,
///          tgt_label, tgt_id, tgt_props
const EXPORT_COLUMN_COUNT: usize = 8;

/// Parse the raw text output of a graph export Cypher query into an
/// [`ExportedGraph`].
///
/// The query must return exactly [`EXPORT_COLUMN_COUNT`] columns per row
/// in the order defined above.
pub fn parse_export_result(raw: &str, scope: ExportScope) -> crate::error::Result<ExportedGraph> {
    let parsed = parse_graph_snapshot(raw).unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let rows = parsed.as_array().cloned().unwrap_or_default();

    let mut nodes_map: HashMap<String, ExportedNode> = HashMap::new();
    let mut edges: Vec<ExportedEdge> = Vec::new();

    for row in &rows {
        let cols = match row.as_array() {
            Some(c) if c.len() >= EXPORT_COLUMN_COUNT => c,
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

        // Insert/update source node.
        if !src_id.is_empty() {
            nodes_map
                .entry(src_id.clone())
                .or_insert_with(|| ExportedNode {
                    display_name: derive_display_name(&src_label, &src_props),
                    id: src_id.clone(),
                    label: src_label.clone(),
                    event_order: parse_event_order(&src_props),
                    properties: src_props.clone(),
                });
        }

        // Insert/update target node.
        if !tgt_id.is_empty() {
            nodes_map
                .entry(tgt_id.clone())
                .or_insert_with(|| ExportedNode {
                    display_name: derive_display_name(&tgt_label, &tgt_props),
                    id: tgt_id.clone(),
                    label: tgt_label.clone(),
                    event_order: parse_event_order(&tgt_props),
                    properties: tgt_props,
                });
        }

        // Record the edge.
        if !src_id.is_empty() && !tgt_id.is_empty() && !rel_type.is_empty() {
            edges.push(ExportedEdge {
                from: src_id,
                to: tgt_id,
                relation: rel_type,
                properties: rel_props,
            });
        }
    }

    // Build a temporal lookup for edge sorting before consuming nodes_map.
    let order_of: HashMap<String, Option<u64>> = nodes_map
        .iter()
        .map(|(id, node)| (id.clone(), node.event_order))
        .collect();

    // Temporal ordering: sort nodes by event_order (None last), then by id for
    // stability. This preserves the causal sequence from the Cypher query's
    // ORDER BY a2a:event_id.
    let mut nodes: Vec<ExportedNode> = nodes_map.into_values().collect();
    nodes.sort_by(|a, b| cmp_event_order(a.event_order, &a.id, b.event_order, &b.id));

    // Sort edges temporally: primary by source event_order, secondary by
    // relation name (grouping), tertiary by target event_order. This ensures
    // that edges fanning out from a hub node (e.g. TaskExecution) appear in
    // the causal order their targets were created.
    edges.sort_by(|a, b| {
        let a_from_ord = order_of.get(a.from.as_str()).copied().flatten();
        let b_from_ord = order_of.get(b.from.as_str()).copied().flatten();
        let a_to_ord = order_of.get(a.to.as_str()).copied().flatten();
        let b_to_ord = order_of.get(b.to.as_str()).copied().flatten();
        cmp_event_order(a_from_ord, &a.from, b_from_ord, &b.from)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| cmp_event_order(a_to_ord, &a.to, b_to_ord, &b.to))
    });

    // Deduplicate edges that appear identically (same from/to/relation).
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to && a.relation == b.relation);

    Ok(ExportedGraph {
        nodes,
        edges,
        scope,
    })
}

// ── Scope post-filtering ────────────────────────────────────────────────────

/// Node labels exempt from scope filtering.
///
/// Agent nodes legitimately cross context boundaries because the boot event
/// fires with a separate context_id from the conversation. Removing them
/// would leave the graph without agent attribution.
const SCOPE_EXEMPT_LABELS: &[&str] = &["AgentRuntimeInstance", "AgentBoot", "AgentArchive"];

/// Post-filter an exported graph to remove nodes that belong to a different
/// scope (context or task).
///
/// The broad Cypher `OR` query can pull in stale nodes via shared endpoints
/// (e.g. a `Message` node whose ID is reused across CLI runs). This function
/// removes those contaminants while preserving agent nodes that legitimately
/// cross context boundaries.
///
/// A node is **kept** if:
/// - Its label is in [`SCOPE_EXEMPT_LABELS`], OR
/// - It has no `property_key` property at all, OR
/// - Its `property_key` value matches `expected_value`.
///
/// Edges referencing any removed node are also dropped.
fn filter_scope(graph: ExportedGraph, property_key: &str, expected_value: &str) -> ExportedGraph {
    let removed: std::collections::HashSet<String> = graph
        .nodes
        .iter()
        .filter(|n| {
            // Exempt agent-related nodes from scope filtering.
            if SCOPE_EXEMPT_LABELS.contains(&n.label.as_str()) {
                return false;
            }
            // Remove if the node has the property but it doesn't match.
            n.properties
                .get(property_key)
                .and_then(|v| v.as_str())
                .is_some_and(|v| v != expected_value)
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
/// Handles real FalkorDB data shapes: JSON arrays for content, raw enum
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
            // Append completion info when available.
            if let Some(duration) = prop_str(a2a::DURATION_MS) {
                name.push_str(&format!(" {duration}ms"));
            }
            if is_success_value(props.get(a2a::SUCCESS)) {
                name.push_str(" ✅");
            } else if is_failure_value(props.get(a2a::SUCCESS)) {
                name.push_str(" ❌");
            }
            name
        }
        Some(GraphNodeLabel::ToolCall) => {
            let tool = strip_tool_prefix(&prop_str(a2a::TOOL_NAME).unwrap_or_default());
            let phase = extract_metadata_field(props.get(a2a::METADATA), "phase");
            match phase {
                Some(p) => format!("🔧 {tool} ({p})"),
                None => format!("🔧 {tool}"),
            }
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
/// `"support/clickupNavigate"` → `"clickupNavigate"`.
fn strip_tool_prefix(name: &str) -> String {
    // Strip known prefixes; the most common is `support/`.
    if let Some(rest) = name.strip_prefix("support/") {
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

/// Check if a JSON value represents a success (bool `true` or string `"true"`).
fn is_success_value(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    }
}

/// Check if a JSON value represents a failure (bool `false` or string `"false"`).
fn is_failure_value(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(false)) => true,
        Some(serde_json::Value::String(s)) => s == "false",
        _ => false,
    }
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
/// Fallback: use `a2a:timestamp_ms` when `event_id` is absent.
fn parse_event_order(props: &HashMap<String, serde_json::Value>) -> Option<u64> {
    // Try a2a:event_id first (format: "prov-{counter}").
    if let Some(event_id) = props.get(a2a::EVENT_ID).and_then(|v| v.as_str())
        && let Some(counter_str) = event_id.strip_prefix("prov-")
        && let Ok(counter) = counter_str.parse::<u64>()
    {
        return Some(counter);
    }
    // Fallback to a2a:timestamp_ms.
    props.get(a2a::TIMESTAMP_MS).and_then(|v| v.as_u64())
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

/// Extract a flat property map from a FalkorDB `properties(n)` column value.
///
/// The column may arrive as a `Map(...)` debug wrapper (already decoded into a
/// JSON object by [`parse_graph_snapshot`]) or as a raw JSON object.
fn extract_properties(v: &serde_json::Value) -> HashMap<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), decode_embedded_json(v)))
            .collect(),
        serde_json::Value::String(s) => {
            // Attempt to parse a JSON-encoded string.
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(s)
            {
                map.into_iter()
                    .map(|(k, v)| (k, decode_embedded_json(&v)))
                    .collect()
            } else {
                HashMap::new()
            }
        }
        _ => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal raw Cypher output row for testing.
    #[allow(clippy::too_many_arguments)]
    fn make_raw_row(
        src_label: &str,
        src_id: &str,
        src_props: &str,
        rel_type: &str,
        rel_props: &str,
        tgt_label: &str,
        tgt_id: &str,
        tgt_props: &str,
    ) -> String {
        format!(
            "[\"{src_label}\", \"{src_id}\", {src_props}, \"{rel_type}\", {rel_props}, \"{tgt_label}\", \"{tgt_id}\", {tgt_props}]"
        )
    }

    #[test]
    fn parse_export_result_empty_input() {
        let graph = parse_export_result("", ExportScope::Full).expect("should parse empty");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn parse_export_result_single_edge() {
        let raw = make_raw_row(
            "ToolCall",
            "prov-1",
            r#"Map({"a2a:tool_name": String("support/clickup")})"#,
            "WAS_USED_BY",
            "Map({})",
            "ToolArgs",
            "prov-2",
            r#"Map({"a2a:args": String("{\"action\":\"CreateTask\"}")})"#,
        );
        let graph =
            parse_export_result(&raw, ExportScope::Context("ctx-1".into())).expect("should parse");

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);

        let tool_node = graph.nodes.iter().find(|n| n.label == "ToolCall").unwrap();
        assert_eq!(tool_node.id, "prov-1");
        // After strip_tool_prefix, "support/clickup" → "clickup".
        assert!(
            tool_node.display_name.contains("clickup"),
            "display_name should contain stripped tool name: {}",
            tool_node.display_name
        );
        assert!(
            !tool_node.display_name.contains("support/"),
            "display_name should not contain 'support/' prefix: {}",
            tool_node.display_name
        );

        // ToolArgs should show a summary of args, not the tool name.
        let args_node = graph.nodes.iter().find(|n| n.label == "ToolArgs").unwrap();
        assert!(
            args_node.display_name.contains("action=CreateTask"),
            "ToolArgs display_name should summarize args: {}",
            args_node.display_name
        );

        let edge = &graph.edges[0];
        assert_eq!(edge.from, "prov-1");
        assert_eq!(edge.to, "prov-2");
        assert_eq!(edge.relation, "WAS_USED_BY");
    }

    #[test]
    fn parse_export_result_deduplicates_nodes() {
        // Same node appears as both source and target in two different rows.
        let row1 = make_raw_row(
            "Message",
            "msg-1",
            r#"Map({"a2a:role": String("user")})"#,
            "WAS_RECEIVED_BY",
            "Map({})",
            "A2AMessageProcessing",
            "mp-1",
            "Map({})",
        );
        let row2 = make_raw_row(
            "A2AMessageProcessing",
            "mp-1",
            "Map({})",
            "WAS_EXECUTED_BY",
            "Map({})",
            "ToolCall",
            "tc-1",
            r#"Map({"a2a:tool_name": String("support/clickup")})"#,
        );
        let raw = format!("{row1}\n{row2}");
        let graph = parse_export_result(&raw, ExportScope::Full).expect("should parse");

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
        let graph = parse_export_result("No results returned.", ExportScope::Full)
            .expect("should parse 'no results'");
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn edges_are_sorted_and_deduped() {
        // Two identical edges should collapse into one.
        let row = make_raw_row(
            "Message",
            "msg-1",
            "Map({})",
            "WAS_RECEIVED_BY",
            "Map({})",
            "A2AMessageProcessing",
            "mp-1",
            "Map({})",
        );
        let raw = format!("{row}\n{row}");
        let graph = parse_export_result(&raw, ExportScope::Full).expect("should parse");
        assert_eq!(graph.edges.len(), 1, "duplicate edges should be deduped");
    }

    // ── Display name enrichment tests (§15 fixes) ───────────────────────

    #[test]
    fn message_content_from_json_array() {
        let mut props = HashMap::new();
        props.insert(
            a2a::ROLE.to_string(),
            serde_json::Value::String("ROLE_USER".to_string()),
        );
        // Content stored as a JSON array (as FalkorDB returns it).
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
        // Metadata stored as a JSON-encoded string (common in FalkorDB).
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
        props.insert(a2a::SUCCESS.to_string(), serde_json::Value::Bool(true));
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
