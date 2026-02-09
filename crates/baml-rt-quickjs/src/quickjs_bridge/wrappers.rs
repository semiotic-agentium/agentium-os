pub(crate) fn build_token_args_wrapper(function_name: &str, invoke_expr: &str) -> String {
    const TOKEN_BLOCK: &str = r#"
                let token = tokenOrArgs;
                let args = rest;
                if (typeof tokenOrArgs !== 'string' || tokenOrArgs.length === 0) {
                    if (tokenOrArgs && typeof tokenOrArgs === 'object' && typeof tokenOrArgs.__baml_invocation_token === 'string') {
                        token = tokenOrArgs.__baml_invocation_token;
                        args = [tokenOrArgs, ...rest];
                    } else {
                        throw new Error("Missing invocation token");
                    }
                }
"#;
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

    format!(
        r#"
            globalThis.{function_name} = async function(tokenOrArgs, ...rest) {{
{token_block}{arg_block}                return await {invoke_expr};
            }};
            "#,
        function_name = function_name,
        token_block = TOKEN_BLOCK,
        arg_block = ARG_BLOCK,
        invoke_expr = invoke_expr
    )
}
