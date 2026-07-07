// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Stamps tool effect metadata for provenance symmetry (scope IDs, agent package).

use baml_rt_core::context;
use baml_rt_tools::ToolRegistry as ConcreteToolRegistry;
use serde_json::Value;

pub(crate) fn stamp_agent_package(agent_package: Option<&str>, metadata: &mut Value) {
    let Some(pkg) = agent_package.filter(|s| !s.is_empty()) else {
        return;
    };
    let Value::Object(obj) = metadata else {
        return;
    };
    obj.insert("agent_package".to_string(), Value::String(pkg.to_string()));
}

/// Align provenance effect metadata with the scope executing the tool.
pub(crate) fn stamp_tool_effect_metadata_scope(
    scope: &context::RuntimeScope,
    metadata: &mut Value,
) {
    let Value::Object(obj) = metadata else {
        return;
    };
    obj.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_string()),
    );
    obj.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_string()),
    );
    if let Some(task_id) = scope.task_id_opt() {
        obj.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
}

/// Stamp registry-derived tier hints for semiotic gate classification.
pub(crate) fn stamp_tool_registry_metadata(
    tool_registry: &ConcreteToolRegistry,
    tool_name: &str,
    metadata: &mut Value,
) {
    let Some(tool_meta) = tool_registry.get_metadata(tool_name) else {
        return;
    };
    let Value::Object(obj) = metadata else {
        return;
    };
    if let Some(access) = tool_meta.access {
        obj.insert(
            "access_level".to_string(),
            Value::String(access.as_str().to_string()),
        );
    }
    if !tool_meta.tags.is_empty()
        && let Ok(tags) = serde_json::to_value(&tool_meta.tags)
    {
        obj.insert("tags".to_string(), tags);
    }
}
