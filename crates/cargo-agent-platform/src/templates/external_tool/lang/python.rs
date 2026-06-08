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
    let (schema_input, schema_output, schema_content_digest) =
        bind_sandbox::starter_schema_parts(ctx);
    // `SUPPORTED_METHODS` is a Rust `&[&str]`; render as a Python list literal
    // so the scaffold stays faithful to the V1 contract without re-typing the
    // method name inside the generated code.
    let supported_methods_py = SUPPORTED_METHODS
        .iter()
        .map(|m| format!("{:?}", m))
        .collect::<Vec<_>>()
        .join(", ");

    let main_py = format!(
        r#"#!/usr/bin/env python3
"""Starter external tool implementation.

Wire contract: one JSON-RPC request on stdin, one JSON-RPC response on
stdout, logs on stderr. Method and error-code constants come from the
runtime crate — keep them in sync if you cross-edit both.
"""
import json
import sys

PROTOCOL_VERSION = "{protocol_version}"
METHOD_DESCRIBE = "{method_describe}"
METHOD_SCHEMA = "{method_schema}"
METHOD_INVOKE = "{method_invoke}"
SUPPORTED_METHODS = [{supported_methods_py}]
DEFAULT_SCHEMA_CONTENT_TYPE = "application/schema+json"
ERR_METHOD_NOT_FOUND = {err_method_not_found}
ERR_PARSE_ERROR = {err_parse_error}
ERR_INTERNAL = {err_internal}

# Starter-contract field names — the schema in tool-manifest.json and this
# handler must stay in lockstep, so both come from the scaffold generator.
INPUT_KEY = "{input_key}"
OUTPUT_KEY = "{output_key}"

# Tool-owned schema. This tool is the single source of truth for its own
# contract; the runner discovers it via `tool/schema` and never reads a schema
# file. `content_digest` is baked at scaffold time with the runner's
# canonicalization, so the runner's discovery-time recomputation matches.
SCHEMA_INPUT = json.loads('{schema_input}')
SCHEMA_OUTPUT = json.loads('{schema_output}')
SCHEMA_CONTENT_DIGEST = "{schema_content_digest}"


def write_result(req_id, result):
    sys.stdout.write(json.dumps({{"jsonrpc": "2.0", "id": req_id, "result": result}}) + "\n")


def write_error(req_id, code, message, error_class="execution"):
    sys.stdout.write(
        json.dumps(
            {{
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {{
                    "code": code,
                    "message": message,
                    "data": {{"error_class": error_class}},
                }},
            }}
        )
        + "\n"
    )


def schema_result():
    return {{
        "schema_version": 1,
        "tool_name": "{tool_id}",
        "content_type": DEFAULT_SCHEMA_CONTENT_TYPE,
        "content_digest": SCHEMA_CONTENT_DIGEST,
        "input": SCHEMA_INPUT,
        "output": SCHEMA_OUTPUT,
    }}


def main():
    raw = sys.stdin.read().strip()
    if not raw:
        write_error(None, ERR_PARSE_ERROR, "empty request", "invalid_argument")
        return

    try:
        req = json.loads(raw)
    except Exception:
        write_error(None, ERR_PARSE_ERROR, "invalid JSON", "invalid_argument")
        return

    req_id = req.get("id")
    method = req.get("method")

    if method == METHOD_DESCRIBE:
        write_result(
            req_id,
            {{
                "protocol_version": PROTOCOL_VERSION,
                "tool_name": "{tool_id}",
                "supported_methods": SUPPORTED_METHODS,
                "schema_digest": SCHEMA_CONTENT_DIGEST,
            }},
        )
        return

    if method == METHOD_SCHEMA:
        write_result(req_id, schema_result())
        return

    if method == METHOD_INVOKE:
        message = req.get("params", {{}}).get("input", {{}}).get(INPUT_KEY, "")
        print(f"invoke {{INPUT_KEY}}={{message}}", file=sys.stderr)
        write_result(req_id, {{"output": {{OUTPUT_KEY: message}}, "done": True}})
        return

    write_error(req_id, ERR_METHOD_NOT_FOUND, f"method not found: {{method}}", "invalid_argument")


if __name__ == "__main__":
    try:
        main()
    except Exception as err:
        print(f"fatal: {{err}}", file=sys.stderr)
        write_error(None, ERR_INTERNAL, "tool execution failure", "execution")
"#,
        protocol_version = PROTOCOL_VERSION,
        method_describe = METHOD_DESCRIBE,
        method_schema = METHOD_SCHEMA,
        method_invoke = METHOD_INVOKE,
        supported_methods_py = supported_methods_py,
        err_method_not_found = ERR_METHOD_NOT_FOUND,
        err_parse_error = ERR_PARSE_ERROR,
        err_internal = ERR_INTERNAL,
        input_key = STARTER_INPUT_KEY,
        output_key = STARTER_OUTPUT_KEY,
    );

    let gitignore = "__pycache__/\n*.pyc\n.venv/\n";

    vec![
        GeneratedFile::new("main.py", main_py),
        GeneratedFile::new("requirements.txt", ""),
        GeneratedFile::new(".gitignore", gitignore),
        tool_server_wrapper("python3 \"$DIR/main.py\""),
    ]
}
