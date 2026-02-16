//! JS wrapper and prelude code generation.
//!
//! Invocation context is host-only (no token/context prelude in JS). Natives resolve
//! scope from the active context stack. This module still provides wrapped promise
//! helpers for eval result tracking.

use baml_rt_core::Result;

/// No prelude: invocation context is resolved on the host from the active context stack.
/// JS never receives tokens or context ids.
#[allow(dead_code)]
pub(crate) fn build_scope_prelude_empty() -> Result<String> {
    Ok(String::new())
}

/// Build the async IIFE that awaits `code_promise_expr` and sets `__set_eval_result(token, json)`.
///
/// Used by the promise-polling path in `evaluate()` so the host can detect when
/// the promise has resolved. `token_literal` must be the eval token string,
/// escaped for embedding in JS (backslash and double-quote escaped).
pub(crate) fn build_wrapped_promise_code(code_promise_expr: &str, token_literal: &str) -> String {
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
        code_promise_expr, token_literal, token_literal
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn proptest_cfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(cases);
        cfg.failure_persistence = None;
        cfg
    }

    #[test]
    fn build_scope_prelude_empty_returns_empty() {
        assert_eq!(build_scope_prelude_empty().unwrap(), "");
    }

    proptest! {
        #![proptest_config(proptest_cfg(32))]

        /// Invariant: Wrapped promise code contains token and __set_eval_result.
        #[test]
        fn prop_build_wrapped_promise_contains_token_and_set_eval(
            code_expr in "[a-zA-Z0-9().,_ ]{1,60}",
            token_literal in "[a-zA-Z0-9-]{1,40}",
        ) {
            let out = build_wrapped_promise_code(&code_expr, &token_literal);
            assert!(out.contains("__set_eval_result"), "output must call __set_eval_result");
            assert!(out.contains(&token_literal), "output must embed token literal");
            assert!(out.contains(&code_expr), "output must embed code expression");
        }
    }
}
