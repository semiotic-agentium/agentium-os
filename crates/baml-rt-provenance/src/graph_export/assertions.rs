//! Test assertion helpers for [`ExportedGraph`].
//!
//! These functions provide semantic assertions over graph structure —
//! node existence, edge counts, path traversal — replacing fragile raw-Cypher
//! string comparisons and monolithic snapshot tests.

use super::{ExportedGraph, ExportedNode};
use std::collections::HashSet;

// ── Path assertion types ────────────────────────────────────────────────────

/// A step in an expected graph path.
#[derive(Debug)]
pub struct PathStep {
    /// Expected node label (e.g. `"ToolCall"`).
    pub label: String,
    /// Optional `(property_key, property_value)` filter on the node.
    pub property_filter: Option<(String, String)>,
}

/// An expected edge connecting two path steps.
#[derive(Debug)]
pub struct PathEdge {
    /// Expected relation type (e.g. `"WAS_EXECUTED_BY"`).
    pub relation: String,
}

/// Convenience constructor for a [`PathStep`].
pub fn step(label: &str, property_filter: Option<(&str, &str)>) -> PathStep {
    PathStep {
        label: label.to_string(),
        property_filter: property_filter.map(|(k, v)| (k.to_string(), v.to_string())),
    }
}

/// Convenience constructor for a [`PathEdge`].
pub fn edge(relation: &str) -> PathEdge {
    PathEdge {
        relation: relation.to_string(),
    }
}

// ── Assertions ──────────────────────────────────────────────────────────────

/// Verify that a path exists in the graph following the given steps and edges.
///
/// `steps` is a list of `(PathStep, Option<PathEdge>)` pairs.  The last step's
/// edge should be `None` (it terminates the path).  For each consecutive pair
/// `(step_i, edge_i) → (step_{i+1}, _)`, the function checks that:
///
/// 1. A node matching `step_i` exists.
/// 2. An edge with relation `edge_i.relation` connects it to a node matching
///    `step_{i+1}`.
///
/// # Panics
///
/// Panics with a descriptive message if the path cannot be found.
pub fn assert_path_exists(graph: &ExportedGraph, steps: &[(PathStep, Option<PathEdge>)]) {
    assert!(
        !steps.is_empty(),
        "assert_path_exists: steps must not be empty"
    );

    // Find all nodes matching each step.
    let step_candidates: Vec<Vec<&ExportedNode>> = steps
        .iter()
        .map(|(s, _)| nodes_matching_step(graph, s))
        .collect();

    // Walk the path: for each step, find a matching candidate reachable from
    // the previous step via the previous step's edge.
    let mut current_id: Option<&str> = None;

    for (i, (step_def, _edge_def)) in steps.iter().enumerate() {
        let candidates = &step_candidates[i];
        assert!(
            !candidates.is_empty(),
            "assert_path_exists: no node matches step {i} (label={}, filter={:?})",
            step_def.label,
            step_def.property_filter,
        );

        if i == 0 {
            // First step: just pick any matching candidate.
            current_id = Some(&candidates[0].id);
            continue;
        }

        // For step i (i > 0), use the edge from step i-1 to reach this step.
        let prev_edge = steps[i - 1].1.as_ref();
        match prev_edge {
            Some(pe) => {
                let cur = current_id.unwrap_or_default();
                let reachable = candidates.iter().find(|c| {
                    graph
                        .edges
                        .iter()
                        .any(|e| e.from == cur && e.to == c.id && e.relation == pe.relation)
                });
                match reachable {
                    Some(node) => current_id = Some(&node.id),
                    None => {
                        panic!(
                            "assert_path_exists: no edge '{}' from current node to step {i} \
                             (label={}, filter={:?}). Current node: {:?}, candidates: {:?}",
                            pe.relation,
                            step_def.label,
                            step_def.property_filter,
                            current_id,
                            candidates.iter().map(|c| &c.id).collect::<Vec<_>>(),
                        );
                    }
                }
            }
            // No edge on the previous step — the path terminated there.
            None => {
                current_id = Some(&candidates[0].id);
            }
        }
    }
}

/// Count nodes matching a label and optional property filter.
pub fn count_nodes(
    graph: &ExportedGraph,
    label: &str,
    property_filter: Option<(&str, &str)>,
) -> usize {
    graph
        .nodes
        .iter()
        .filter(|n| {
            n.label == label
                && property_filter.is_none_or(|(k, v)| {
                    n.properties.get(k).and_then(|val| val.as_str()) == Some(v)
                })
        })
        .count()
}

/// Count edges matching a relation type.
pub fn count_edges(graph: &ExportedGraph, relation: &str) -> usize {
    graph
        .edges
        .iter()
        .filter(|e| e.relation == relation)
        .count()
}

/// Assert that a node with the given label and all specified properties exists.
///
/// # Panics
///
/// Panics if no matching node is found.
pub fn assert_node_exists(graph: &ExportedGraph, label: &str, properties: &[(&str, &str)]) {
    let found = graph.nodes.iter().any(|n| {
        n.label == label
            && properties
                .iter()
                .all(|(k, v)| n.properties.get(*k).and_then(|val| val.as_str()) == Some(*v))
    });
    assert!(
        found,
        "Expected node with label={label} and properties={properties:?} not found in graph.\n\
         Available nodes: {:?}",
        graph
            .nodes
            .iter()
            .map(|n| format!("{}({})", n.label, n.id))
            .collect::<Vec<_>>(),
    );
}

/// Return the set of node ids that are not connected to any edge.
///
/// An isolated node participates in zero edges (neither as source nor target).
pub fn find_isolated_nodes(graph: &ExportedGraph) -> Vec<&str> {
    let connected: HashSet<&str> = graph
        .edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();

    graph
        .nodes
        .iter()
        .filter(|n| !connected.contains(n.id.as_str()))
        .map(|n| n.id.as_str())
        .collect()
}

/// Assert that the graph contains no isolated nodes.
///
/// # Panics
///
/// Panics with a list of isolated node ids if any exist.
pub fn assert_no_isolated_nodes(graph: &ExportedGraph) {
    let isolated = find_isolated_nodes(graph);
    assert!(
        isolated.is_empty(),
        "Found isolated nodes (not connected to any edge): {isolated:?}"
    );
}

// ── Private helpers ─────────────────────────────────────────────────────────

fn nodes_matching_step<'a>(graph: &'a ExportedGraph, step: &PathStep) -> Vec<&'a ExportedNode> {
    graph
        .nodes
        .iter()
        .filter(|n| {
            n.label == step.label
                && step.property_filter.as_ref().is_none_or(|(k, v)| {
                    n.properties.get(k).and_then(|val| val.as_str()) == Some(v.as_str())
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode};
    use std::collections::HashMap;

    fn make_node(id: &str, label: &str, props: &[(&str, &str)]) -> ExportedNode {
        ExportedNode {
            id: id.to_string(),
            label: label.to_string(),
            display_name: format!("{label} {id}"),
            properties: props
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect(),
            event_order: None,
        }
    }

    fn make_edge(from: &str, rel: &str, to: &str) -> ExportedEdge {
        ExportedEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: rel.to_string(),
            properties: HashMap::new(),
        }
    }

    fn sample_graph() -> ExportedGraph {
        ExportedGraph {
            nodes: vec![
                make_node("msg-1", "Message", &[("a2a:role", "user")]),
                make_node("mp-1", "A2AMessageProcessing", &[]),
                make_node("tc-1", "ToolCall", &[("a2a:tool_name", "support/clickup")]),
                make_node("args-1", "ToolArgs", &[]),
            ],
            edges: vec![
                make_edge("msg-1", "WAS_RECEIVED_BY", "mp-1"),
                make_edge("mp-1", "WAS_EXECUTED_BY", "tc-1"),
                make_edge("tc-1", "WAS_USED_BY", "args-1"),
            ],
            scope: ExportScope::Full,
        }
    }

    #[test]
    fn count_nodes_by_label() {
        let g = sample_graph();
        assert_eq!(count_nodes(&g, "Message", None), 1);
        assert_eq!(count_nodes(&g, "ToolCall", None), 1);
        assert_eq!(count_nodes(&g, "NonExistent", None), 0);
    }

    #[test]
    fn count_nodes_with_property_filter() {
        let g = sample_graph();
        assert_eq!(
            count_nodes(&g, "ToolCall", Some(("a2a:tool_name", "support/clickup"))),
            1
        );
        assert_eq!(
            count_nodes(&g, "ToolCall", Some(("a2a:tool_name", "other"))),
            0
        );
    }

    #[test]
    fn count_edges_by_relation() {
        let g = sample_graph();
        assert_eq!(count_edges(&g, "WAS_RECEIVED_BY"), 1);
        assert_eq!(count_edges(&g, "WAS_EXECUTED_BY"), 1);
        assert_eq!(count_edges(&g, "WAS_USED_BY"), 1);
        assert_eq!(count_edges(&g, "NONEXISTENT"), 0);
    }

    #[test]
    fn assert_node_exists_succeeds() {
        let g = sample_graph();
        assert_node_exists(&g, "Message", &[("a2a:role", "user")]);
        assert_node_exists(&g, "ToolCall", &[("a2a:tool_name", "support/clickup")]);
    }

    #[test]
    #[should_panic(expected = "Expected node with label=ToolCall")]
    fn assert_node_exists_fails() {
        let g = sample_graph();
        assert_node_exists(&g, "ToolCall", &[("a2a:tool_name", "wrong_tool")]);
    }

    #[test]
    fn find_isolated_detects_disconnected_node() {
        let mut g = sample_graph();
        g.nodes.push(make_node("orphan-1", "LlmPrompt", &[]));
        let isolated = find_isolated_nodes(&g);
        assert_eq!(isolated, vec!["orphan-1"]);
    }

    #[test]
    fn find_isolated_empty_for_connected_graph() {
        let g = sample_graph();
        assert!(find_isolated_nodes(&g).is_empty());
    }

    #[test]
    fn assert_path_exists_succeeds() {
        let g = sample_graph();
        assert_path_exists(
            &g,
            &[
                (
                    step("Message", Some(("a2a:role", "user"))),
                    Some(edge("WAS_RECEIVED_BY")),
                ),
                (
                    step("A2AMessageProcessing", None),
                    Some(edge("WAS_EXECUTED_BY")),
                ),
                (
                    step("ToolCall", Some(("a2a:tool_name", "support/clickup"))),
                    None,
                ),
            ],
        );
    }

    #[test]
    #[should_panic(expected = "no edge")]
    fn assert_path_exists_fails_on_missing_edge() {
        let g = sample_graph();
        // Try to find a direct WAS_EXECUTED_BY from Message to ToolCall (doesn't exist).
        assert_path_exists(
            &g,
            &[
                (step("Message", None), Some(edge("WAS_EXECUTED_BY"))),
                (step("ToolCall", None), None),
            ],
        );
    }
}
