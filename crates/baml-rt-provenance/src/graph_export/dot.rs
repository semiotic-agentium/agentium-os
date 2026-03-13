//! Graphviz DOT renderer for [`ExportedGraph`].
//!
//! Produces a DOT `digraph` string that can be rendered to SVG/PDF via the
//! `dot` command-line tool or embedded via `@viz-js/viz` WASM.

use std::{collections::BTreeMap, fmt::Write};

use super::ExportedGraph;
use crate::graph_model::GraphNodeLabel;

// ── Options ─────────────────────────────────────────────────────────────────

/// Configuration for the DOT renderer.
#[derive(Debug, Clone)]
pub struct DotOptions {
    /// Graphviz layout engine hint (dot, neato, fdp, etc.).
    pub layout: DotLayout,
    /// Whether to include edge labels.
    pub show_edge_labels: bool,
    /// Whether to cluster nodes by type.
    pub cluster_by_type: bool,
}

impl Default for DotOptions {
    fn default() -> Self {
        Self {
            layout: DotLayout::Dot,
            show_edge_labels: true,
            cluster_by_type: false,
        }
    }
}

/// Graphviz layout engine.
#[derive(Debug, Clone, Default)]
pub enum DotLayout {
    #[default]
    Dot,
    Neato,
    Fdp,
}

impl DotLayout {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dot => "dot",
            Self::Neato => "neato",
            Self::Fdp => "fdp",
        }
    }
}

// ── Renderer ────────────────────────────────────────────────────────────────

/// Render an [`ExportedGraph`] as a Graphviz DOT string.
pub fn render_dot(graph: &ExportedGraph, options: &DotOptions) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "digraph provenance {{");
    let _ = writeln!(out, "    layout={};", options.layout.as_str());
    let _ = writeln!(out, "    rankdir=TB;");
    let _ = writeln!(
        out,
        "    node [fontname=\"Helvetica\", fontsize=10, style=filled, fontcolor=\"#0f172a\"];"
    );
    let _ = writeln!(out, "    edge [fontname=\"Helvetica\", fontsize=9];");
    let _ = writeln!(out);

    if options.cluster_by_type {
        let mut by_label: BTreeMap<String, Vec<(&str, &str)>> = BTreeMap::new();
        for node in &graph.nodes {
            by_label
                .entry(node.label.clone())
                .or_default()
                .push((&node.id, &node.display_name));
        }
        for (i, (label, nodes)) in by_label.iter().enumerate() {
            let escaped_label = escape_dot_label(label);
            let _ = writeln!(out, "    subgraph cluster_{i} {{");
            let _ = writeln!(out, "        label=\"{escaped_label}\";");
            for (id, display) in nodes {
                let (shape, color) = dot_node_attrs(label);
                let escaped_id = escape_dot_label(id);
                let escaped = escape_dot_label(display);
                let _ = writeln!(
                    out,
                    "        \"{escaped_id}\" [label=\"{escaped}\", shape={shape}, fillcolor=\"{color}\"];"
                );
            }
            let _ = writeln!(out, "    }}");
        }
    } else {
        for node in &graph.nodes {
            let (shape, color) = dot_node_attrs(&node.label);
            let escaped_id = escape_dot_label(&node.id);
            let escaped = escape_dot_label(&node.display_name);
            let _ = writeln!(
                out,
                "    \"{escaped_id}\" [label=\"{escaped}\", shape={shape}, fillcolor=\"{color}\"];",
            );
        }
    }

    let _ = writeln!(out);

    for edge in &graph.edges {
        let escaped_from = escape_dot_label(&edge.from);
        let escaped_to = escape_dot_label(&edge.to);
        if options.show_edge_labels {
            let escaped_relation = escape_dot_label(&edge.relation);
            let _ = writeln!(
                out,
                "    \"{escaped_from}\" -> \"{escaped_to}\" [label=\"{escaped_relation}\"];",
            );
        } else {
            let _ = writeln!(out, "    \"{escaped_from}\" -> \"{escaped_to}\";");
        }
    }

    let _ = writeln!(out, "}}");
    out
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Return `(shape, fillcolor)` for a Graphviz node based on its label.
fn dot_node_attrs(label: &str) -> (&'static str, &'static str) {
    match GraphNodeLabel::parse(label) {
        Some(
            GraphNodeLabel::LlmCall
            | GraphNodeLabel::ToolCall
            | GraphNodeLabel::MessageProcessing
            | GraphNodeLabel::TaskExecution
            | GraphNodeLabel::AgentBoot,
        ) => ("ellipse", "#dbeafe"),
        Some(GraphNodeLabel::AgentRuntimeInstance) => ("hexagon", "#e2e8f0"),
        Some(
            GraphNodeLabel::Intent
            | GraphNodeLabel::Plan
            | GraphNodeLabel::PlanStep
            | GraphNodeLabel::Message
            | GraphNodeLabel::LlmPrompt
            | GraphNodeLabel::ToolArgs
            | GraphNodeLabel::Task
            | GraphNodeLabel::TaskState
            | GraphNodeLabel::Artifact
            | GraphNodeLabel::AgentArchive
            | GraphNodeLabel::PromptRejected
            | GraphNodeLabel::FailureClassificationActivity
            | GraphNodeLabel::FailureClassification,
        ) => ("box", "#ecfeff"),
        None => ("box", "#f1f5f9"),
    }
}

/// Escape characters that are special in DOT label strings.
fn escape_dot_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode};

    fn node(id: &str, label: &str, display: &str) -> ExportedNode {
        ExportedNode {
            id: id.to_string(),
            label: label.to_string(),
            display_name: display.to_string(),
            properties: HashMap::new(),
            event_order: None,
        }
    }

    fn edge(from: &str, rel: &str, to: &str) -> ExportedEdge {
        ExportedEdge {
            from: from.to_string(),
            to: to.to_string(),
            relation: rel.to_string(),
            properties: HashMap::new(),
        }
    }

    #[test]
    fn render_empty_graph() {
        let graph = ExportedGraph {
            nodes: vec![],
            edges: vec![],
            scope: ExportScope::Full,
        };
        let output = render_dot(&graph, &DotOptions::default());
        assert!(output.contains("digraph provenance"));
        assert!(output.contains("layout=dot"));
    }

    #[test]
    fn render_simple_graph() {
        let graph = ExportedGraph {
            nodes: vec![
                node("msg-1", "Message", "user: Hello"),
                node("tc-1", "ToolCall", "Tool clickup"),
            ],
            edges: vec![edge("msg-1", "WAS_RECEIVED_BY", "tc-1")],
            scope: ExportScope::Full,
        };
        let output = render_dot(&graph, &DotOptions::default());
        assert!(output.contains("\"msg-1\""));
        assert!(output.contains("\"tc-1\""));
        assert!(output.contains("\"msg-1\" -> \"tc-1\""));
        assert!(output.contains("WAS_RECEIVED_BY"));
    }

    #[test]
    fn render_clustered() {
        let graph = ExportedGraph {
            nodes: vec![
                node("tc-1", "ToolCall", "Tool a"),
                node("tc-2", "ToolCall", "Tool b"),
                node("msg-1", "Message", "msg"),
            ],
            edges: vec![],
            scope: ExportScope::Full,
        };
        let opts = DotOptions {
            cluster_by_type: true,
            ..DotOptions::default()
        };
        let output = render_dot(&graph, &opts);
        assert!(output.contains("subgraph cluster_"));
        assert!(output.contains("label=\"ToolCall\""));
    }

    #[test]
    fn render_dot_escapes_ids_and_edge_labels() {
        let graph = ExportedGraph {
            nodes: vec![
                node("msg\"1\nline", "Message", "user: Hello"),
                node("tc\"2", "ToolCall", "Tool clickup"),
            ],
            edges: vec![edge("msg\"1\nline", "WAS_\"RECEIVED\"\nBY", "tc\"2")],
            scope: ExportScope::Full,
        };
        let output = render_dot(&graph, &DotOptions::default());
        assert!(output.contains("\"msg\\\"1\\nline\""));
        assert!(output.contains("\"tc\\\"2\""));
        assert!(output.contains("label=\"WAS_\\\"RECEIVED\\\"\\nBY\""));
    }
}
