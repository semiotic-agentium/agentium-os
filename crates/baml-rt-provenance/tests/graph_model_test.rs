// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_provenance::{
    ALL_EVENT_KINDS, EDGE_WAS_USED_BY, EventGraphKind, GraphNodeLabel, TOOL_CALL_ARGS_EDGE,
    mapping_for_event_kind,
    vocabulary::{a2a_roles, a2a_types, prov},
};

#[test]
fn all_event_kinds_have_mapping_specs() {
    for kind in ALL_EVENT_KINDS {
        let mapping = mapping_for_event_kind(kind);
        assert_eq!(mapping.kind, kind);
        assert!(
            !mapping.required_properties.is_empty(),
            "missing required properties for {kind:?}"
        );
    }
}

#[test]
fn tool_call_args_edge_contract_is_typed_and_stable() {
    assert_eq!(TOOL_CALL_ARGS_EDGE.edge_label, EDGE_WAS_USED_BY);
    assert_eq!(TOOL_CALL_ARGS_EDGE.role_key, prov::ROLE);
    assert_eq!(TOOL_CALL_ARGS_EDGE.role_value, a2a_roles::ARGS);
    assert_eq!(TOOL_CALL_ARGS_EDGE.target_type_key, prov::TYPE);
    assert_eq!(TOOL_CALL_ARGS_EDGE.target_type_value, a2a_types::TOOL_ARGS);
}

#[test]
fn tool_call_event_mappings_use_tool_call_primary_node() {
    let started = mapping_for_event_kind(EventGraphKind::ToolCallStarted);
    let completed = mapping_for_event_kind(EventGraphKind::ToolCallCompleted);
    assert_eq!(started.primary_node, GraphNodeLabel::ToolCall);
    assert_eq!(completed.primary_node, GraphNodeLabel::ToolCall);
}
