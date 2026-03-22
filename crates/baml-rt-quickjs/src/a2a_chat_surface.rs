//! Canonical QuickJS global names for the A2A **chat host** surface.
//!
//! All registration of these identifiers in
//! [`crate::quickjs_bridge::QuickJSBridge::register_baml_functions`] must go through these
//! constants so [`crate::quickjs_bridge::QuickJSBridge::verify_a2a_chat_host_surface`] cannot
//! drift from the actual `set_function` / `eval` wiring (execution-session + step-executor
//! natives, session helpers, and core invoke/stream shims).

/// Tokenless single-shot BAML invoke from JS.
pub const BAML_INVOKE: &str = "__baml_invoke";
/// Tokenless BAML stream invoke from JS.
pub const BAML_STREAM: &str = "__baml_stream";
/// Await a promise and JSON-stringify the outcome (installed via `eval`).
pub const AWAIT_AND_STRINGIFY: &str = "__awaitAndStringify";
/// Promise detection helper (installed via `eval`).
pub const IS_PROMISE: &str = "__isPromise";
/// Internal: stores eval promise results by token.
pub const SET_EVAL_RESULT: &str = "__set_eval_result";
/// Validates step-executor FSM transitions in Rust.
pub const STEP_EXECUTOR_VALIDATE_TRANSITION: &str = "__step_executor_validate_transition";
/// Resolves `tool_name` → polymorphic step executor function name.
pub const RESOLVE_TOOL_STEP_EXECUTOR: &str = "__resolve_tool_step_executor";
/// Rust-hosted multi-hop step executor loop.
pub const RUN_STEP_EXECUTOR: &str = "__run_step_executor";
/// Execution-session command dispatch (`Open`, `submitIntent`, …).
pub const EXECUTION_SESSION_INVOKE: &str = "__execution_session_invoke";
/// Session-scoped `__baml_invoke` (explicit `session_id`).
pub const BAML_INVOKE_SESSION: &str = "__baml_invoke_session";
/// Session-scoped `__baml_stream`.
pub const BAML_STREAM_SESSION: &str = "__baml_stream_session";

/// Ordered list matching the **foundation** phase of
/// [`crate::quickjs_bridge::QuickJSBridge::register_baml_functions`] (before per-function
/// wrappers and tool registration).
pub const A2A_CHAT_HOST_GLOBALS: &[&str] = &[
    BAML_INVOKE,
    BAML_STREAM,
    AWAIT_AND_STRINGIFY,
    IS_PROMISE,
    SET_EVAL_RESULT,
    STEP_EXECUTOR_VALIDATE_TRANSITION,
    RESOLVE_TOOL_STEP_EXECUTOR,
    RUN_STEP_EXECUTOR,
    EXECUTION_SESSION_INVOKE,
    BAML_INVOKE_SESSION,
    BAML_STREAM_SESSION,
];

/// JavaScript expression (no trailing semicolon) that `eval_sync` can run; evaluates to a JSON
/// **string** containing `{ "ok": bool, "bad": string[] }`.
pub fn host_surface_probe_expression() -> String {
    let names_json = serde_json::to_string(A2A_CHAT_HOST_GLOBALS)
        .expect("A2A_CHAT_HOST_GLOBALS must serialize to JSON");
    format!(
        r#"(function() {{
            const names = {names_json};
            const bad = [];
            for (const n of names) {{
                const t = typeof globalThis[n];
                if (t !== 'function') {{
                    bad.push(n + ':' + t);
                }}
            }}
            return JSON.stringify({{ ok: bad.length === 0, bad }});
        }})()"#
    )
}
