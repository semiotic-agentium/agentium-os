//! JS wrapper and prelude code generation.
//!
//! Shared helpers for building JavaScript prelude strings (scope, token) and
//! wrapped promise expressions used by the bridge and stream paths. Ensures
//! token and scope are bound consistently for eval.

use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::context::InvocationScope;
use serde::Serialize;

/// Serialize an ID to a JSON string for JavaScript prelude code.
pub(crate) fn serialize_id(id: &impl Serialize) -> Result<String> {
    serde_json::to_string(id).map_err(BamlRtError::Json)
}

/// Build the scope + token prelude string for eval.
///
/// Binds `__baml_invocation_token`, `__baml_context_id`, `__baml_message_id`,
/// and `__baml_task_id` so JS and native callbacks can use them. Token prelude
/// must be the single line that defines `const __baml_invocation_token = "..."`.
pub(crate) fn build_scope_prelude(scope: &InvocationScope, token_prelude: &str) -> Result<String> {
    let context_prelude = format!(
        "const __baml_context_id = {};",
        serialize_id(&scope.context_id)?
    );
    let message_prelude = match scope.message_id.as_ref() {
        Some(id) => format!("const __baml_message_id = {};", serialize_id(id)?),
        None => "const __baml_message_id = undefined;".to_string(),
    };
    let task_prelude = match scope.task_id.as_ref() {
        Some(id) => format!("const __baml_task_id = {};", serialize_id(id)?),
        None => "const __baml_task_id = undefined;".to_string(),
    };
    Ok(format!(
        "{token_prelude}\n{context_prelude}\n{message_prelude}\n{task_prelude}",
        token_prelude = token_prelude,
        context_prelude = context_prelude,
        message_prelude = message_prelude,
        task_prelude = task_prelude
    ))
}

/// Build the async IIFE that awaits `code_promise_expr` and sets `__set_eval_result(token, json)`.
///
/// Used by the promise-polling path in `evaluate()` so the host can detect when
/// the promise has resolved. `token_literal` must be the eval token string,
/// escaped for embedding in JS (backslash and double-quote escaped).
pub(crate) fn build_wrapped_promise_code(
    code_promise_expr: &str,
    token_literal: &str,
) -> String {
    format!(
        r#"
            (async function() {{
                try {{
                    const codePromise = {};
                    const result = await codePromise;
                    const json = (typeof result === 'string' && result.length > 0)
                        ? result
                        : JSON.stringify({{ error: (result === undefined ? 'promise resolved with undefined' : String(result)) }});
                    __set_eval_result("{}", json);
                }} catch (error) {{
                    __set_eval_result("{}", JSON.stringify({{ error: error.toString() }}));
                }}
            }})()
            "#,
        code_promise_expr,
        token_literal,
        token_literal
    )
}
