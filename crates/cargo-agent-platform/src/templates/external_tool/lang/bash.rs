// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_tools::external_tools::{
    ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA,
    PROTOCOL_VERSION, SUPPORTED_METHODS,
};

use super::super::{
    GeneratedFile, STARTER_INPUT_KEY, STARTER_OUTPUT_KEY, ScaffoldContext, bind_sandbox,
};

pub fn generate(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    let tool_id = ctx.tool_id();
    // Tool-owned schema + a digest computed with the runner's canonicalization,
    // so the scaffolded tool's `tool/schema` response validates at discovery.
    let (schema_input, schema_output, schema_content_digest) =
        bind_sandbox::starter_schema_parts(ctx);
    // Inline the methods array as a JSON literal so jq can splat it into the
    // describe response without a second parse hop.
    let supported_methods_json = serde_json::to_string(SUPPORTED_METHODS)
        .expect("SUPPORTED_METHODS serializes as JSON array");

    // Protocol parity with the Rust/Python/TS scaffolds: detect empty stdin,
    // invalid JSON, and missing `.id` / `.method` before dispatching. Each
    // failure path emits a proper JSON-RPC error frame on stdout so the runner
    // can classify the failure instead of hitting a broken pipe.
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
set -o pipefail

emit_error() {{
  local req_id="$1"
  local code="$2"
  local message="$3"
  local klass="${{4:-execution}}"
  jq -nc \
    --argjson id "$req_id" \
    --argjson code "$code" \
    --arg message "$message" \
    --arg klass "$klass" \
    '{{
      jsonrpc: "2.0",
      id: $id,
      error: {{
        code: $code,
        message: $message,
        data: {{ error_class: $klass }}
      }}
    }}'
}}

# Read exactly one JSON-RPC frame from stdin. `read` returns non-zero on EOF
# even when the line was captured, so fall back to `-eof` semantics.
request=""
if ! IFS= read -r request; then
  : # EOF before newline is fine; $request may still hold a payload.
fi

if [[ -z "${{request// }}" ]]; then
  emit_error null {err_parse_error} "empty request" "invalid_argument"
  exit 0
fi

# Validate JSON before any `.id` / `.method` probe so a parse error still
# returns a structured frame rather than letting jq explode mid-pipeline.
if ! printf '%s' "$request" | jq -e . >/dev/null 2>&1; then
  emit_error null {err_parse_error} "invalid JSON" "invalid_argument"
  exit 0
fi

# Preserve JSON type for id (string/number/null). `-c` keeps valid JSON
# literals so `--argjson id "$id"` works for all JSON-RPC id variants.
id=$(jq -c '.id // null' <<<"$request")
[[ -z "$id" ]] && id=null

method=$(jq -r '.method // ""' <<<"$request")

case "$method" in
  "{method_describe}")
    jq -nc \
      --argjson id "$id" \
      --arg tool_name "{tool_id}" \
      --arg protocol_version "{protocol_version}" \
      --arg schema_digest "{schema_content_digest}" \
      --argjson supported_methods '{supported_methods_json}' \
      '{{
        jsonrpc: "2.0",
        id: $id,
        result: {{
          protocol_version: $protocol_version,
          tool_name: $tool_name,
          supported_methods: $supported_methods,
          schema_digest: $schema_digest
        }}
      }}'
    ;;

  "{method_schema}")
    # Tool-owned schema. Discovered via tool/schema; never read from a file.
    # content_digest baked at scaffold time with the runner's canonicalization.
    jq -nc \
      --argjson id "$id" \
      --arg tool_name "{tool_id}" \
      --arg content_digest "{schema_content_digest}" \
      --argjson schema_input '{schema_input}' \
      --argjson schema_output '{schema_output}' \
      '{{
        jsonrpc: "2.0",
        id: $id,
        result: {{
          schema_version: 1,
          tool_name: $tool_name,
          content_type: "application/schema+json",
          content_digest: $content_digest,
          input: $schema_input,
          output: $schema_output
        }}
      }}'
    ;;

  "{method_invoke}")
    message=$(jq -r '.params.input.{input_key} // ""' <<<"$request")
    # stderr-only — protocol contract keeps stdout for the frame.
    echo "invoke {input_key}=$message" >&2
    jq -nc \
      --argjson id "$id" \
      --arg echoed "$message" \
      '{{
        jsonrpc: "2.0",
        id: $id,
        result: {{
          output: {{ {output_key}: $echoed }},
          done: true
        }}
      }}'
    ;;

  *)
    emit_error "$id" {err_method_not_found} "method not found: $method" "invalid_argument"
    ;;
esac
"#,
        method_describe = METHOD_DESCRIBE,
        method_schema = METHOD_SCHEMA,
        method_invoke = METHOD_INVOKE,
        protocol_version = PROTOCOL_VERSION,
        err_method_not_found = ERR_METHOD_NOT_FOUND,
        err_parse_error = ERR_PARSE_ERROR,
        input_key = STARTER_INPUT_KEY,
        output_key = STARTER_OUTPUT_KEY,
    );

    vec![GeneratedFile::new("tool-server", script).executable()]
}
