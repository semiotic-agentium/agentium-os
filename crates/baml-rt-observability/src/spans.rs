//! OpenTelemetry span helpers for baml-rt
//!
//! This module provides structured span instrumentation following the OTel guide pattern.
//! All span names use the `baml_rt.` namespace prefix for low cardinality.
//!
//! Root spans include runtime scope attributes (context_id, message_id, task_id) when available.
//! Child spans inherit context automatically through OTEL span nesting - no need to repeat attributes.

use crate::scope::scope_attributes;
use baml_rt_core::context::RuntimeScope;
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
pub fn invoke_function(
    scope: Option<&RuntimeScope>,
    agent_name: &str,
    function_name: &str,
) -> Span {
    let correlation_id = current_correlation_id()
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let (context_id, message_id, task_id) = scope_attributes(scope);
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
pub fn a2a_request(scope: Option<&RuntimeScope>, method: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, task_id) = scope_attributes(scope);
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
pub fn a2a_stream(scope: Option<&RuntimeScope>, method: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, task_id) = scope_attributes(scope);
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
pub fn a2a_cancel(scope: Option<&RuntimeScope>, task_id: &str, correlation_id: &str) -> Span {
    let (context_id, message_id, _) = scope_attributes(scope);
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

// A2A stdio loop and routing

/// Create span for one A2A request in the stdio loop.
///
/// Parent: (none; root per request)
#[inline]
pub fn a2a_stdio_request(agent_name: &str, method: &str, correlation_id: &str) -> Span {
    tracing::info_span!(
        "baml_rt.a2a_stdio_request",
        agent = agent_name,
        method = method,
        correlation_id = correlation_id,
    )
}

/// Create span for A2A routing and dispatch (method-based router).
///
/// Parent: a2a_request / a2a_stream / a2a_stdio_request
#[inline]
pub fn a2a_route(method: &str, context_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt.a2a_route",
        method = method,
        context_id = context_id,
    )
}

/// Create span for JS handler invocation (onChatMessage / stream).
///
/// Parent: a2a_route
#[inline]
pub fn a2a_js_invoke(method: &str, is_stream: bool) -> Span {
    tracing::debug_span!(
        "baml_rt.a2a_js_invoke",
        method = method,
        is_stream = is_stream,
    )
}

/// Create span for A2A worker drain on QuickJS worker thread.
///
/// Not parented to caller (runs on different thread).
#[inline]
pub fn a2a_worker_drain() -> Span {
    tracing::debug_span!("baml_rt.a2a_worker_drain")
}

/// Create span for session dispatcher loop.
#[inline]
pub fn session_dispatcher() -> Span {
    tracing::debug_span!("baml_rt.session_dispatcher")
}

/// Create span for session runtime worker loop.
#[inline]
pub fn session_runtime_worker() -> Span {
    tracing::debug_span!("baml_rt.session_runtime_worker")
}

/// Create span for a single provenance event write.
///
/// Parent: (caller; e.g. interceptor or effect subscriber)
#[inline]
pub fn provenance_write(event_kind: &str) -> Span {
    tracing::debug_span!("baml_rt.provenance_write", event_kind = event_kind,)
}
