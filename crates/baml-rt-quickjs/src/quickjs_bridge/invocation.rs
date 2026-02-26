//! Invocation JS code generation.
//!
//! Helpers that build the JS strings for __baml_invoke, __baml_stream,
//! __js_tools[name], and globalThis[function_name] invocations.
//! Registration of natives lives in baml_registration; this module only
//! generates the call-site code.

/// Build IIFE that calls __baml_invoke(function_name, JSON.stringify(args)) and __awaitAndStringify.
pub(crate) fn build_baml_invoke_js_code(function_name: &str, args_json: &str) -> String {
    format!(
        r#"
        (function() {{
            try {{
                const args = {};
                const promise = __baml_invoke("{}", JSON.stringify(args));
                return __awaitAndStringify(promise);
            }} catch (error) {{
                return JSON.stringify({{ error: error.message || String(error) }});
            }}
        }})()
        "#,
        args_json, function_name
    )
}

/// Build IIFE that gets globalThis.__js_tools[name], calls it with args, and __awaitAndStringify.
pub(crate) fn build_js_tool_invoke_js_code(tool_name: &str, args_json: &str) -> String {
    format!(
        r#"
        (function() {{
            try {{
                const args = {};
                const func = globalThis.__js_tools && globalThis.__js_tools["{}"];
                if (func === undefined || typeof func !== 'function') {{
                    return JSON.stringify({{ error: "JS tool not found" }});
                }}
                return __awaitAndStringify(func(args));
            }} catch (error) {{
                return JSON.stringify({{ error: error.message || String(error) }});
            }}
        }})()
        "#,
        args_json, tool_name
    )
}

/// Build IIFE that gets globalThis[function_name], calls it with args, and __awaitAndStringify.
pub(crate) fn build_js_function_invoke_js_code(function_name: &str, args_json: &str) -> String {
    format!(
        r#"
        (function() {{
            try {{
                const args = {};
                const func = globalThis["{}"];
                if (func === undefined || typeof func !== 'function') {{
                    return JSON.stringify({{ error: "JS function not found: {}" }});
                }}
                return __awaitAndStringify(func(args));
            }} catch (error) {{
                return JSON.stringify({{ error: error.message || String(error) }});
            }}
        }})()
        "#,
        args_json, function_name, function_name
    )
}

/// Build IIFE for optional JS function: returns __absent when not found, else __awaitAndStringify.
pub(crate) fn build_optional_js_function_invoke_js_code(
    function_name: &str,
    args_json: &str,
) -> String {
    format!(
        r#"
        (function() {{
            try {{
                const args = {};
                const func = globalThis["{}"];
                if (func === undefined || typeof func !== 'function') {{
                    return JSON.stringify({{ __absent: true }});
                }}
                return __awaitAndStringify(func(args));
            }} catch (error) {{
                return JSON.stringify({{ error: error.message || String(error) }});
            }}
        }})()
        "#,
        args_json, function_name
    )
}

/// Build IIFE for stream: tries globalThis["{}Stream"] then __baml_stream(function_name, args).
pub(crate) fn build_stream_invoke_js_code(function_name: &str, args_json: &str) -> String {
    let stream_function = format!("{}Stream", function_name);
    format!(
        r#"
        (function() {{
            try {{
                const args = {};
                let promise;
                const streamFunc = globalThis["{}"];
                if (streamFunc !== undefined && typeof streamFunc === 'function') {{
                    promise = streamFunc(args);
                }} else {{
                    promise = __baml_stream("{}", JSON.stringify(args));
                }}
                return __awaitAndStringify(promise);
            }} catch (error) {{
                return JSON.stringify({{ error: error.message || String(error) }});
            }}
        }})()
        "#,
        args_json, stream_function, function_name
    )
}
