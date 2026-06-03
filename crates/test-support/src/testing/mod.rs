// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Matrix-test fixtures, snapshot macros, and shared assertion helpers.

pub mod provenance_fixtures;

pub use baml_rt_provenance::graph_export::{
    ExportedGraph,
    assertions::{
        assert_no_isolated_nodes, assert_node_exists, assert_path_exists, count_edges, count_nodes,
        edge, step,
    },
};
