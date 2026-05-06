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

/// Concatenate per-tool invoke wrappers into one script (single `eval` vs N× `eval`).
pub(crate) fn build_tool_invoke_wrappers_batch(tool_names: &[String]) -> String {
    tool_names
        .iter()
        .map(|tool_name| {
            let escaped = tool_name.replace('\\', "\\\\").replace('"', "\\\"");
            build_token_args_wrapper(
                tool_name,
                &format!("__tool_invoke(\"{escaped}\", JSON.stringify(argObj))"),
            )
        })
        .collect()
}

/// Batch register `globalThis[name]` → `__baml_invoke(name, ...)`.
pub(crate) fn build_baml_invoke_wrappers_batch(function_names: &[String]) -> String {
    function_names
        .iter()
        .map(|function_name| {
            let escaped = function_name.replace('\\', "\\\\").replace('"', "\\\"");
            build_token_args_wrapper(
                function_name,
                &format!("__baml_invoke(\"{escaped}\", JSON.stringify(argObj))"),
            )
        })
        .collect()
}

/// Batch register `globalThis[nameStream]` → `__baml_stream(name, ...)`.
pub(crate) fn build_baml_stream_wrappers_batch(function_names: &[String]) -> String {
    function_names
        .iter()
        .map(|function_name| {
            let stream_function_name = format!("{}Stream", function_name);
            let escaped = function_name.replace('\\', "\\\\").replace('"', "\\\"");
            build_token_args_wrapper(
                &stream_function_name,
                &format!("__baml_stream(\"{escaped}\", JSON.stringify(argObj))"),
            )
        })
        .collect()
}
