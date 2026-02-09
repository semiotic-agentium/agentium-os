//! OpenTelemetry span helpers for baml-rt
//!
//! This module provides structured span instrumentation following the OTel guide pattern.
//! All span names use the `baml_rt.` namespace prefix for low cardinality.
//!
//! Root spans include runtime scope attributes (context_id, message_id, task_id) when available.
//! Child spans inherit context automatically through OTEL span nesting - no need to repeat attributes.

use crate::scope::scope_attributes;
use baml_rt_core::correlation::current_correlation_id;
use std::path::Path;
use tracing::Span;

// Builder operations

/// Create span for agent linting operation.
///
/// Parent: CLI command span
#[inline]
pub fn lint_agent(agent_dir: &Path) -> Span {
    tracing::debug_span!(
        "baml_rt.lint_agent",
        agent_dir = %agent_dir.display(),
    )
}

/// Create span for agent packaging operation.
///
/// Parent: CLI command span
/// Children: compile_typescript, generate_types, package_create
#[inline]
pub fn package_agent(agent_dir: &Path, output: &Path) -> Span {
    tracing::info_span!(
        "baml_rt.package_agent",
        agent_dir = %agent_dir.display(),
        output = %output.display(),
    )
}

/// Create span for TypeScript compilation.
///
/// Parent: package_agent
#[inline]
pub fn compile_typescript(src_dir: &Path, dist_dir: &Path) -> Span {
    tracing::debug_span!(
        "baml_rt.compile_typescript",
        src_dir = %src_dir.display(),
        dist_dir = %dist_dir.display(),
    )
}

/// Create span for type generation.
///
/// Parent: package_agent
#[inline]
pub fn generate_types(baml_src: &Path) -> Span {
    tracing::debug_span!(
        "baml_rt.generate_types",
        baml_src = %baml_src.display(),
    )
}

// Agent loading and execution

/// Create span for loading an agent package.
///
/// Parent: CLI command span
/// Children: load_baml_schema, create_js_bridge, evaluate_agent_code
#[inline]
pub fn load_agent_package(package_path: &Path) -> Span {
    tracing::info_span!(
        "baml_rt.load_agent_package",
        package_path = %package_path.display(),
    )
}

/// Create span for extracting agent package archive.
///
/// Parent: load_agent_package
#[inline]
pub fn extract_package(extract_dir: &Path) -> Span {
    tracing::debug_span!(
        "baml_rt.extract_package",
        extract_dir = %extract_dir.display(),
    )
}

/// Create span for loading BAML schema.
///
/// Parent: load_agent_package
#[inline]
pub fn load_baml_schema(schema_path: &Path) -> Span {
    tracing::debug_span!(
        "baml_rt.load_baml_schema",
        schema_path = %schema_path.display(),
    )
}

/// Create span for creating QuickJS bridge.
///
/// Parent: load_agent_package
#[inline]
pub fn create_js_bridge() -> Span {
    tracing::debug_span!("baml_rt.create_js_bridge")
}

/// Create span for registering BAML functions with QuickJS.
///
/// Parent: create_js_bridge
#[inline]
pub fn register_baml_functions(function_count: usize) -> Span {
    tracing::debug_span!(
        "baml_rt.register_baml_functions",
        function_count = function_count,
    )
}

/// Create span for evaluating agent JavaScript code.
///
/// Parent: load_agent_package
#[inline]
pub fn evaluate_agent_code(entry_point: &str) -> Span {
    tracing::debug_span!("baml_rt.evaluate_agent_code", entry_point = entry_point,)
}

/// Create span for invoking an agent function.
///
/// Parent: CLI command span or interactive loop
/// Children: invoke_js_function, invoke_baml_function
///
/// This is a root span - includes runtime scope attributes for context propagation.
#[inline]
pub fn invoke_function(agent_name: &str, function_name: &str) -> Span {
    let correlation_id = current_correlation_id()
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let (context_id, message_id, task_id) = scope_attributes();
    tracing::info_span!(
        "baml_rt.invoke_function",
        agent = agent_name,
        function = function_name,
        correlation_id = correlation_id,
        context_id = %context_id.as_deref().unwrap_or("none"),
        message_id = %message_id.as_deref().unwrap_or("none"),
        task_id = %task_id.as_deref().unwrap_or("none"),
    )
}

/// Create span for JavaScript evaluation in QuickJS.
///
/// Parent: evaluate_agent_code or invoke_function
#[inline]
pub fn evaluate_javascript() -> Span {
    tracing::trace_span!("baml_rt.evaluate_javascript")
}

/// Create span for JavaScript function invocation.
///
/// Parent: invoke_function
/// Children: invoke_baml_function, llm_call, tool_call
///
/// Child span - inherits context from parent automatically.
#[inline]
pub fn invoke_js_function(function_name: &str) -> Span {
    let correlation_id = current_correlation_id()
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    tracing::debug_span!(
        "baml_rt.invoke_js_function",
        function = function_name,
        correlation_id = correlation_id,
    )
}

/// Create span for BAML function invocation.
///
/// Parent: invoke_function or invoke_js_function
/// Children: llm_call, tool_call
///
/// Child span - inherits context from parent automatically.
#[inline]
pub fn invoke_baml_function(function_name: &str) -> Span {
    let correlation_id = current_correlation_id()
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    tracing::debug_span!(
        "baml_rt.invoke_baml_function",
        function = function_name,
        correlation_id = correlation_id,
    )
}

/// Create span for handling an A2A request.
///
/// Root span - includes runtime scope attributes for context propagation.
#[inline]
pub fn a2a_request(method: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, task_id) = scope_attributes();
    tracing::info_span!(
        "baml_rt.a2a_request",
        method = method,
        correlation_id = correlation_id,
        context_id = %context_id.as_deref().unwrap_or("none"),
        message_id = %message_id.as_deref().unwrap_or("none"),
        task_id = %task_id.as_deref().unwrap_or("none"),
    )
}

/// Create span for handling an A2A stream request.
///
/// Root span - includes runtime scope attributes for context propagation.
#[inline]
pub fn a2a_stream(method: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, task_id) = scope_attributes();
    tracing::info_span!(
        "baml_rt.a2a_stream",
        method = method,
        correlation_id = correlation_id,
        context_id = %context_id.as_deref().unwrap_or("none"),
        message_id = %message_id.as_deref().unwrap_or("none"),
        task_id = %task_id.as_deref().unwrap_or("none"),
    )
}

/// Create span for handling an A2A cancel request.
///
/// Root span - includes runtime scope attributes for context propagation.
#[inline]
pub fn a2a_cancel(task_id: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, _) = scope_attributes();
    tracing::info_span!(
        "baml_rt.a2a_cancel",
        task_id = task_id,
        correlation_id = correlation_id,
        context_id = %context_id.as_deref().unwrap_or("none"),
        message_id = %message_id.as_deref().unwrap_or("none"),
    )
}

/// Create span for registering a tool with QuickJS.
///
/// Parent: create_js_bridge
#[inline]
pub fn register_tool(tool_name: &str) -> Span {
    tracing::debug_span!("baml_rt.register_tool", tool = tool_name,)
}

/// Create span for BAML runtime initialization.
///
/// Parent: load_baml_schema
#[inline]
pub fn init_baml_runtime() -> Span {
    tracing::info_span!("baml_rt.init_baml_runtime")
}
