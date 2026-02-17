/// Tool wrapper: takes only args (no token). Host resolves invocation context from active context stack.
pub(crate) fn build_token_args_wrapper(function_name: &str, invoke_expr: &str) -> String {
    const ARG_BLOCK: &str = r#"
                const argObj = {};
                if (args.length === 1 && typeof args[0] === 'object') {
                    Object.assign(argObj, args[0]);
                } else {
                    args.forEach((arg, idx) => {
                        argObj[`arg${idx}`] = arg;
                    });
                }
"#;

    let key_escaped = function_name.replace('\\', "\\\\").replace('"', "\\\"");

    format!(
        r#"
            globalThis["{key_escaped}"] = async function(...args) {{
{arg_block}                return await {invoke_expr};
            }};
            "#,
        key_escaped = key_escaped,
        arg_block = ARG_BLOCK,
        invoke_expr = invoke_expr
    )
}
