// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
