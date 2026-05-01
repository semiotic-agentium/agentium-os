//! BAML runtime wrapper and function execution.
//!
//! [`BamlRuntimeManager`] owns the function registry, tool registry, and session state.
//!
//! ## Public surface (`pub mod baml` / `baml_rt::baml::*`)
//! The facade [`BamlRuntimeManager`] and [`BamlRuntimeManagerBuilder`] are **stable integration
//! points**. Many methods are public for hosts (QuickJS bridge, tests, coordinators). In
//! particular, [`BamlRuntimeManager::execute_tool_from_baml_result_or_value`] is a **wide
//! contract**: it routes session plans, polymorphic tools, and one-shot calls — signature or
//! semantics changes ripple to multiple crates. Prefer adding **new** entrypoints over breaking
//! this one; internal routing is split via the `tool_invocation_plan` module for clarity.
//!
//! `pub(crate)` modules ([`tool_extraction`], [`runtime_io`], [`open_input`], etc.) are **not**
//! cross-crate API.
//!
//! ## Module layout
//! - [`tool_extraction`] — session-plan and tool-call extraction from BAML JSON.
//! - [`open_input`] — tool `open_input` defaults and JSON-schema checks.
//! - [`runtime_io`] — manifest load, delegation extract, tool-session trace helpers.
//! - [`builder`] — [`BamlRuntimeManagerBuilder`].
//! - [`state`] — [`BamlRuntimeState`] (`pub(in crate::baml)` fields for real submodules).
//! - `constructor`, `planning_bridge`, `schema_invoke`, `registry`, … — `BamlRuntimeManager` impl shards.
//! - [`tool_session_plan`] — tool session plan FSM.

pub(crate) mod tool_extraction;

mod builder;
mod constructor;
mod intra_turn;
pub(crate) use intra_turn::{append_step_intra_deltas, await_provider_conversation_strict_growth};
mod manager_prelude;
mod manager_traits;
mod open_input;
mod planning_bridge;
mod planning_emit;
mod register_tool;
mod registry;
pub(crate) mod runtime_io;
mod schema_invoke;
mod state;
mod tool_dispatch;
mod tool_invocation_plan;
mod tool_session_plan;
mod tool_sessions;

pub use baml_rt_tools::{SessionPlanFunctionsMap, SessionPlanTypeName};
pub use builder::BamlRuntimeManagerBuilder;
pub(crate) use runtime_io::{
    completion_error_from, extract_delegation_target_from_open_input, tool_session_trace,
    tool_session_trace_enabled,
};
pub(crate) use tool_extraction::{
    ToolSessionOp, ToolSessionPlan, extract_tool_call, normalize_plan_input,
    resolve_tool_name_from_input_with_registry,
};

pub use crate::{
    function_tool_manifest::FunctionToolManifest,
    planning::{
        IntentSubmission, PlanStepStatusChange, PlanSubmission, PlanningDynamicContext,
        PlanningResolver,
    },
    tool_session_handle::ToolSessionExecutionHandle,
};

/// Manages the BAML runtime and function registry.
///
/// Internal fields live in [`state::BamlRuntimeState`] with `pub(in crate::baml)` visibility so
/// implementation can be split across real submodules without `include!`.
#[derive(Default)]
pub struct BamlRuntimeManager {
    pub(in crate::baml) state: state::BamlRuntimeState,
}
