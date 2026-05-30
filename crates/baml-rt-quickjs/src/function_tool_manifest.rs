// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Eagerly resolved function-to-tool manifest.
//!
//! Maps BAML function names to their bound tool names at schema load time.
//! Single-tool functions resolve eagerly; polymorphic functions (multiple
//! session plan candidates) defer resolution to the LLM's Open step selection.

use std::collections::HashMap;

use baml_rt_tools::{SessionPlanFunctionsMap, ToolName, ToolRegistry as ConcreteToolRegistry};

use crate::baml::tool_extraction::resolve_tool_name_from_plan_type_with_registry;

/// Eagerly resolved manifest mapping BAML function names to their bound
/// tool names. Built once at schema load from `session_plan_functions.json`
/// + `ToolRegistry`. Shared immutably for the lifetime of the agent.
#[derive(Debug, Clone, Default)]
pub struct FunctionToolManifest {
    bindings: HashMap<String, ToolName>,
}

impl FunctionToolManifest {
    /// Build from the raw session plan functions map + live registry.
    /// Resolves every entry eagerly — load-time validation.
    /// For single-tool functions (one candidate), resolves that tool's name.
    /// For polymorphic functions (multiple candidates), skips — tool is resolved at Open time.
    pub fn build(raw: &SessionPlanFunctionsMap, registry: &ConcreteToolRegistry) -> Self {
        let mut bindings = HashMap::with_capacity(raw.len());
        for (func_name, candidates) in raw {
            if candidates.len() == 1 {
                match resolve_tool_name_from_plan_type_with_registry(registry, &candidates[0]) {
                    Ok(tool_name) => {
                        bindings.insert(func_name.clone(), tool_name);
                    }
                    Err(e) => {
                        tracing::warn!(
                            function = func_name,
                            plan_type = %candidates[0],
                            error = %e,
                            "FunctionToolManifest: skipping unresolvable binding"
                        );
                    }
                }
            }
        }
        Self { bindings }
    }

    /// Look up the tool name for a BAML function.
    pub fn tool_name_for_function(&self, function_name: &str) -> Option<&ToolName> {
        self.bindings.get(function_name)
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}
