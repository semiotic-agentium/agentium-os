//! Parse BAML JSON into a structured plan before execution (separates routing from side effects).
//!
//! [`resolve_baml_tool_invocation_plan`] is synchronous JSON + manifest work; the caller executes
//! via match on [`BamlToolInvocationPlan`] (see `tool_dispatch`).

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result, context};
use baml_rt_tools::{ToolName, ToolRegistry as ConcreteToolRegistry};
use serde_json::Value;

use super::{
    SessionPlanFunctionsMap, ToolSessionExecutionHandle,
    tool_extraction::{
        ToolSessionPlan, extract_tool_call, extract_tool_session_plan,
        resolve_tool_name_from_input_with_registry, resolve_tool_name_from_plan_type_with_registry,
    },
};

/// Result of classifying a BAML function output for tool execution.
#[derive(Debug)]
pub(crate) enum BamlToolInvocationPlan {
    /// No tool session plan and no extractable tool call — return payload unchanged.
    Passthrough(Value),
    /// Run a global archive-table read by visible @N, independent of selected/open tool state.
    ArchiveRead { plan: ToolSessionPlan },
    /// Run the session FSM (`Open` / `Send` / …).
    SessionPlan {
        tool_name: ToolName,
        plan: ToolSessionPlan,
        source_baml_function: Option<String>,
        invocation_args: Option<Value>,
    },
    /// Single tool invocation from typed args.
    OneShot { tool_name: ToolName, args: Value },
}

/// Classify `baml_result` without executing (stable boundary for tests and alternate executors).
pub(crate) fn resolve_baml_tool_invocation_plan(
    scope: &context::RuntimeScope,
    handle: &ToolSessionExecutionHandle,
    baml_result: Value,
    source_baml_function: Option<&str>,
    invocation_args: Option<&Value>,
    session_plan_functions: &Option<SessionPlanFunctionsMap>,
    tool_registry: &Arc<ConcreteToolRegistry>,
) -> Result<BamlToolInvocationPlan> {
    let plan_result = extract_tool_session_plan(&baml_result).map_err(|e| {
        tracing::warn!(
            error = %e,
            source_function = ?source_baml_function,
            "Tool session plan extraction failed; LLM effect completed with rejection_reason and PromptRejected emitted in provenance"
        );
        e
    })?;
    if let Some(plan) = plan_result {
        if plan.is_archive_read() {
            return Ok(BamlToolInvocationPlan::ArchiveRead { plan });
        }

        let tool_name = if let (Some(func_name), Some(map)) =
            (source_baml_function, session_plan_functions.as_ref())
        {
            if let Some(candidates) = map.get(func_name) {
                match candidates.as_slice() {
                    [single] => {
                        resolve_tool_name_from_plan_type_with_registry(tool_registry, single).ok()
                    }
                    _ => plan
                        .selected_tool
                        .clone()
                        .or_else(|| handle.tool_name_for_scope(scope)),
                }
            } else {
                None
            }
        } else {
            None
        };
        let tool_name = tool_name.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "Session plan tool could not be resolved: no manifest entry for the invoking function, or polymorphic Open missing tool_name. Build the agent with the builder so session_plan_functions.json is present and up to date.".to_string(),
            )
        })?;
        return Ok(BamlToolInvocationPlan::SessionPlan {
            tool_name,
            plan,
            source_baml_function: source_baml_function.map(str::to_owned),
            invocation_args: invocation_args.cloned(),
        });
    }
    if let Some(call) = extract_tool_call(&baml_result)? {
        let tool_name = resolve_tool_name_from_input_with_registry(tool_registry, &call.args)?;
        return Ok(BamlToolInvocationPlan::OneShot {
            tool_name,
            args: call.args,
        });
    }
    Ok(BamlToolInvocationPlan::Passthrough(baml_result))
}
