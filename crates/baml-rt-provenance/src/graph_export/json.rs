//! JSON renderer for [`ExportedGraph`].
//!
//! Serializes the graph to JSON for consumption by frontend libraries
//! (React Flow, Cytoscape.js, D3-force, etc.).

use super::ExportedGraph;

/// Serialize an [`ExportedGraph`] to a JSON string.
///
/// When `pretty` is `true`, the output is indented for human readability.
/// Otherwise it is compact (single line).
pub fn render_json(graph: &ExportedGraph, pretty: bool) -> Result<String, serde_json::Error> {
    if pretty {
        serde_json::to_string_pretty(graph)
    } else {
        serde_json::to_string(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_export::{ExportScope, ExportedEdge, ExportedGraph, ExportedNode};
    use std::collections::HashMap;

    fn sample_graph() -> ExportedGraph {
        ExportedGraph {
            nodes: vec![ExportedNode {
                id: "msg-1".to_string(),
                label: "Message".to_string(),
                display_name: "📩 user: Hello".to_string(),
                properties: HashMap::new(),
                event_order: None,
            }],
            edges: vec![ExportedEdge {
                from: "msg-1".to_string(),
                to: "mp-1".to_string(),
                relation: "WAS_RECEIVED_BY".to_string(),
                properties: HashMap::new(),
            }],
            scope: ExportScope::Context("ctx-1".to_string()),
        }
    }

    #[test]
    fn render_json_compact() {
        let json = render_json(&sample_graph(), false).expect("should serialize");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should parse");
        assert!(parsed["nodes"].is_array());
        assert!(parsed["edges"].is_array());
    }

    #[test]
    fn render_json_pretty() {
        let json = render_json(&sample_graph(), true).expect("should serialize");
        assert!(json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should parse");
        assert_eq!(parsed["scope"]["Context"], "ctx-1");
    }

    #[test]
    fn json_roundtrip() {
        let original = sample_graph();
        let json = render_json(&original, false).expect("should serialize");
        let restored: ExportedGraph = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(original, restored);
    }
}
