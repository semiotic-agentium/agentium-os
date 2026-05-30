// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Test assertion helpers for [`ExportedGraph`].
//!
//! These functions provide semantic assertions over graph structure —
//! node existence, edge counts, path traversal — replacing fragile raw-query
//! string comparisons and monolithic snapshot tests.

use std::collections::HashSet;

use super::{ExportedGraph, ExportedNode};

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
