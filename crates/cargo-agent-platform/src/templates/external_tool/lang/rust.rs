// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_tools::external_tools::{
    ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, METHOD_DESCRIBE, METHOD_INVOKE,
    METHOD_SCHEMA, PROTOCOL_VERSION, SUPPORTED_METHODS,
};

use super::super::{
    GeneratedFile, STARTER_INPUT_KEY, STARTER_OUTPUT_KEY, ScaffoldContext, bind_sandbox,
    tool_server_wrapper,
};

pub fn generate(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    let tool_id = ctx.tool_id();
    // Tool-owned schema + a digest computed with the runner's canonicalization,
    // so the scaffolded tool's `tool/schema` response validates at discovery.
    // The compact JSON is valid `json!` syntax, so it embeds without escaping.
    let (schema_input, schema_output, schema_content_digest) =
        bind_sandbox::starter_schema_parts(ctx);
    // Render SUPPORTED_METHODS as an inline Rust slice literal (e.g.
    // `["tool/invoke"]`). Keeps the source of truth in `templates/mod.rs`
    // instead of re-declaring method names inside generated code.
    let supported_methods_rs = format!(
        "[{}]",
        SUPPORTED_METHODS
            .iter()
            .map(|m| format!("{:?}", m))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Match workspace edition so the scaffold inherits current-era features
    // (let-chains, async-fn-in-trait, etc.) without warnings.
    let cargo_toml = r#"[package]
name = "external-tool"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
"#;

    let main_rs = format!(
        r#"//! Starter external tool implementation (Rust).
//!
//! Wire contract: one JSON-RPC request on stdin, one response on stdout,
//! logs on stderr. Protocol constants mirror `baml_rt_tools::external_tools`
//! so cross-edits stay in sync.

use std::io::{{self, Read}};

use serde_json::json;

const PROTOCOL_VERSION: &str = "{protocol_version}";
const METHOD_DESCRIBE: &str = "{method_describe}";
const METHOD_SCHEMA: &str = "{method_schema}";
const METHOD_INVOKE: &str = "{method_invoke}";
// Matches scaffold's `SUPPORTED_METHODS`. Describe response echoes this
// so a caller knows which methods the tool handles.
const SUPPORTED_METHODS: &[&str] = &{supported_methods_rs};
const ERR_METHOD_NOT_FOUND: i32 = {err_method_not_found};
const ERR_PARSE_ERROR: i32 = {err_parse_error};
const ERR_INTERNAL: i32 = {err_internal};

// Starter-contract field names — schema in tool-manifest.json and this
// handler must agree, so both come from the scaffold generator.
const INPUT_KEY: &str = "{input_key}";
const OUTPUT_KEY: &str = "{output_key}";

// Tool-owned schema. This tool is the single source of truth for its own
// contract; the runner discovers it via `tool/schema` and never reads a schema
// file. `content_digest` is baked at scaffold time with the runner's
// canonicalization, so the runner's discovery-time recomputation matches.
const SCHEMA_CONTENT_DIGEST: &str = "{schema_content_digest}";

fn schema_result() -> serde_json::Value {{
    json!({{
        "schema_version": 1,
        "tool_name": "{tool_id}",
        "content_type": "application/schema+json",
        "content_digest": SCHEMA_CONTENT_DIGEST,
        "input": {schema_input},
        "output": {schema_output}
    }})
}}

fn write_result(id: serde_json::Value, result: serde_json::Value) {{
    let response = json!({{ "jsonrpc": "2.0", "id": id, "result": result }});
    println!("{{}}", response);
}}

fn write_error(id: serde_json::Value, code: i32, message: &str, error_class: &str) {{
    let response = json!({{
        "jsonrpc": "2.0",
        "id": id,
        "error": {{
            "code": code,
            "message": message,
            "data": {{ "error_class": error_class }}
        }}
    }});
    println!("{{}}", response);
}}

fn main() {{
    let mut raw = String::new();
    if let Err(err) = io::stdin().read_to_string(&mut raw) {{
        eprintln!("failed to read stdin: {{err}}");
        write_error(serde_json::Value::Null, ERR_INTERNAL, "stdin read failure", "execution");
        return;
    }}

    let trimmed = raw.trim();
    if trimmed.is_empty() {{
        write_error(serde_json::Value::Null, ERR_PARSE_ERROR, "empty request", "invalid_argument");
        return;
    }}

    let request: serde_json::Value = match serde_json::from_str(trimmed) {{
        Ok(v) => v,
        Err(_) => {{
            write_error(serde_json::Value::Null, ERR_PARSE_ERROR, "invalid JSON", "invalid_argument");
            return;
        }}
    }};

    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    match method {{
        m if m == METHOD_DESCRIBE => {{
            write_result(
                id,
                json!({{
                    "protocol_version": PROTOCOL_VERSION,
                    "tool_name": "{tool_id}",
                    "supported_methods": SUPPORTED_METHODS,
                    "schema_digest": SCHEMA_CONTENT_DIGEST
                }}),
            );
        }}
        m if m == METHOD_SCHEMA => {{
            write_result(id, schema_result());
        }}
        m if m == METHOD_INVOKE => {{
            let message = request
                .get("params")
                .and_then(|v| v.get("input"))
                .and_then(|v| v.get(INPUT_KEY))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            eprintln!("invoke {{INPUT_KEY}}={{message}}");
            write_result(id, json!({{ "output": {{ OUTPUT_KEY: message }}, "done": true }}));
        }}
        _ => write_error(
            id,
            ERR_METHOD_NOT_FOUND,
            &format!("method not found: {{method}}"),
            "invalid_argument",
        ),
    }}
}}
"#,
        protocol_version = PROTOCOL_VERSION,
        method_describe = METHOD_DESCRIBE,
        method_schema = METHOD_SCHEMA,
        method_invoke = METHOD_INVOKE,
        supported_methods_rs = supported_methods_rs,
        err_method_not_found = ERR_METHOD_NOT_FOUND,
        err_parse_error = ERR_PARSE_ERROR,
        err_internal = ERR_INTERNAL,
        input_key = STARTER_INPUT_KEY,
        output_key = STARTER_OUTPUT_KEY,
    );

    // `cargo run` re-checks dependencies every invoke; for hot paths
    // switch to the release binary after the first `cargo build --release`.
    let tool_server_exec = r#"cargo run --quiet --manifest-path "$DIR/Cargo.toml" --"#;

    let gitignore = "target/\n";

    vec![
        GeneratedFile::new("Cargo.toml", cargo_toml),
        GeneratedFile::new("src/main.rs", main_rs),
        GeneratedFile::new(".gitignore", gitignore),
        tool_server_wrapper(tool_server_exec),
    ]
}
