// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared scaffold helpers for sandbox bind setup scripts.
//!
//! Docker-assisted artifacts are optional and only emitted when
//! `new-tool --runtime sandbox --sandbox-source bind --generate-docker`.

use baml_rt_tools::external_tools::{
    ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PAYLOAD_LIMIT_EXCEEDED,
    ERR_SCHEMA_DIGEST_MISMATCH, ERR_SIDECAR_MALFORMED, ERR_SIDECAR_MISSING,
    ERR_SIDECAR_SCHEMA_INVALID, ERR_SIDECAR_SIZE_EXCEEDED, ERR_UNSUPPORTED_PROTOCOL,
    METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, render_sidecar_bundle,
};

use super::{
    GeneratedFile, Language, STARTER_INPUT_KEY, STARTER_OUTPUT_KEY, ScaffoldContext, manifest_json,
};
pub fn files(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    if !ctx.generate_docker {
        return Vec::new();
    }

    vec![
        GeneratedFile::new("setup_bind_sandbox.sh", setup_script_with_docker(ctx)).executable(),
        GeneratedFile::new("inspect_tool.py", inspect_tsrpc_script(ctx)).executable(),
        GeneratedFile::new("adapter/Dockerfile", dockerfile(ctx)),
        GeneratedFile::new("adapter/tool-adapter", adapter_script(ctx)).executable(),
        GeneratedFile::new(
            "adapter/sidecars/etc/agentium/tool-bundle.json",
            scaffold_sidecar_bundle(ctx),
        ),
    ]
}

fn setup_script_with_docker(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

# Bind sandbox setup helper (Docker-assisted mode).
#
# This script delegates to `sandbox-bind-sync`, which:
#   1) builds adapter image from adapter/Dockerfile
#   2) exports image filesystem into a bind rootfs directory
#   3) writes tool-manifest.lock.json (gitignored) with the host bind path
#   4) validates metadata via `check-external-tool` (with --check)

run_agent_platform() {{
  local subcmd="${{1:-}}"
  shift || true

  if [[ -n "${{AGENT_PLATFORM_CMD:-}}" ]]; then
    # shellcheck disable=SC2206
    local cmd=( $AGENT_PLATFORM_CMD )
    "${{cmd[@]}}" "$subcmd" "$@"
    return
  fi

  if cargo agent-platform "$subcmd" --help >/dev/null 2>&1; then
    cargo agent-platform "$subcmd" "$@"
    return
  fi

  if cargo run -q -p cargo-agent-platform -- "$subcmd" --help >/dev/null 2>&1; then
    cargo run -q -p cargo-agent-platform -- "$subcmd" "$@"
    return
  fi

  cat >&2 <<'EOF'
Could not find a compatible cargo-agent-platform command.

Tried:
  1) cargo agent-platform <subcommand>
  2) cargo run -q -p cargo-agent-platform -- <subcommand>

You can override command resolution with:
  export AGENT_PLATFORM_CMD='cargo run -q -p cargo-agent-platform --'
(or another explicit command that supports sandbox-bind-sync)
EOF
  exit 1
}}

IMAGE="{default_image}"
ROOTFS=""
FORCE=0

usage() {{
  cat <<EOF
setup_bind_sandbox.sh

Usage:
  ./setup_bind_sandbox.sh [--image name:tag] [--rootfs /abs/path|rel/path] [--force]

Options:
  --image <name:tag>   Docker image tag to build/export (default: {default_image})
  --rootfs <dir>       Bind rootfs output directory
                       (default: <tool-dir>/.tmp/{rootfs_dir})
  --force              Recreate rootfs output directory before export
  -h, --help           Show this help
EOF
}}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image)
      IMAGE="$2"
      shift 2
      ;;
    --rootfs)
      ROOTFS="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

TOOL_DIR="$(cd "$(dirname "$0")" && pwd)"
DOCKERFILE="$TOOL_DIR/adapter/Dockerfile"

if [[ ! -f "$DOCKERFILE" ]]; then
  echo "Missing adapter Dockerfile: $DOCKERFILE" >&2
  exit 1
fi

if [[ -z "$ROOTFS" ]]; then
  ROOTFS="$TOOL_DIR/.tmp/{rootfs_dir}"
fi

for bin in docker tar; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "Missing required dependency: $bin" >&2
    exit 1
  fi
done

args=(sandbox-bind-sync --tool-dir "$TOOL_DIR" --rootfs "$ROOTFS" --dockerfile "$DOCKERFILE" --image "$IMAGE" --check)
if [[ $FORCE -eq 1 ]]; then
  args+=(--force)
fi

run_agent_platform "${{args[@]}}"

echo "Bind runtime lock written and validated."
echo "  tool:           {tool_id}"
echo "  image:          $IMAGE"
echo "  bind path:      $ROOTFS"
"#,
        default_image = default_image_tag(ctx),
        rootfs_dir = default_rootfs_dir(ctx),
        tool_id = ctx.tool_id(),
    )
}

fn inspect_tsrpc_script(ctx: &ScaffoldContext<'_>) -> String {
    let template = r#"#!/usr/bin/env python3
"""Inspect tool adapter contract.

All modes send framed TSRPC JSON-RPC to an adapter command you provide after
`--`. The adapter forwards `tool/describe`, `tool/schema`, and `tool/invoke`
to the child tool, which is the live schema source. `tool-bundle.json` only
tells the shim which child to launch; it does not carry schema.

Examples:
  ./inspect_tool.py describe -- docker run --rm -i __DEFAULT_IMAGE__ /tool-adapter
  ./inspect_tool.py schema -- docker run --rm -i __DEFAULT_IMAGE__ /tool-adapter
  ./inspect_tool.py invoke --input '{"__STARTER_INPUT_KEY__":"hello"}' -- docker run --rm -i __DEFAULT_IMAGE__ /tool-adapter
"""

import argparse
import json
import struct
import subprocess

TOOL_ID = __TOOL_ID__


def _build_describe_request() -> dict:
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tool/describe",
        "params": {"tool_name": TOOL_ID},
    }


def _build_schema_request() -> dict:
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tool/schema",
        "params": {"tool_name": TOOL_ID},
    }


def _build_invoke_request(input_json: str) -> dict:
    try:
        input_obj = json.loads(input_json)
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"invalid --input JSON: {exc}")

    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tool/invoke",
        "params": {
            "invocation_id": "inspect",
            "tool_name": TOOL_ID,
            "input": input_obj,
        },
    }


def _invoke_via_adapter(req: dict, cmd: list[str]) -> dict:
    body = json.dumps(req, separators=(",", ":")).encode("utf-8")
    frame = struct.pack(">I", len(body)) + body

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    out, err = proc.communicate(frame, timeout=15)

    if proc.returncode != 0:
        stderr_text = err.decode("utf-8", errors="replace").strip()
        raise SystemExit(
            f"adapter command failed (exit={proc.returncode}): {stderr_text or '(no stderr)'}"
        )

    if len(out) < 4:
        raise SystemExit(f"short framed output: {len(out)} bytes")

    (size,) = struct.unpack(">I", out[:4])
    payload = out[4 : 4 + size]
    if len(payload) != size:
        raise SystemExit(f"truncated payload: expected {size} bytes, got {len(payload)}")

    try:
        return json.loads(payload.decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"response payload is not valid JSON: {exc}")


def _parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["describe", "schema", "invoke"])
    parser.add_argument(
        "--input",
        default='{"__STARTER_INPUT_KEY__":"hello"}',
        help="invoke input JSON (used only for mode=invoke)",
    )
    parser.add_argument(
        "cmd",
        nargs=argparse.REMAINDER,
        help="adapter command (pass after --)",
    )
    args = parser.parse_args()

    cmd = list(args.cmd or [])
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]

    return args, cmd


def main() -> int:
    args, cmd = _parse_args()

    if not cmd:
        raise SystemExit(
            "all modes require adapter command after '--' "
            "(e.g. docker run --rm -i __DEFAULT_IMAGE__ /tool-adapter)"
        )

    if args.mode == "describe":
        req = _build_describe_request()
    elif args.mode == "schema":
        req = _build_schema_request()
    else:
        req = _build_invoke_request(args.input)

    resp = _invoke_via_adapter(req, cmd)
    print(json.dumps(resp, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#;

    template
        .replace("__TOOL_ID__", &format!("{:?}", ctx.tool_id()))
        .replace("__ROOTFS_DIR__", &default_rootfs_dir(ctx))
        .replace(
            "__ROOTFS_DIR_PY__",
            &format!("{:?}", default_rootfs_dir(ctx)),
        )
        .replace("__STARTER_INPUT_KEY__", STARTER_INPUT_KEY)
        .replace("__DEFAULT_IMAGE__", &default_image_tag(ctx))
}

fn dockerfile(ctx: &ScaffoldContext<'_>) -> String {
    let tool_id = ctx.tool_id();

    match ctx.language {
        Language::Python => format!(
            r#"# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for {tool_id} (python source as tool logic).

FROM python:3.12-slim

WORKDIR /opt/tool
COPY main.py /opt/tool/main.py
COPY adapter/tool-adapter /tool-adapter
COPY adapter/sidecars/etc/agentium/tool-bundle.json /etc/agentium/tool-bundle.json

RUN chmod +x /tool-adapter /opt/tool/main.py

ENTRYPOINT ["/tool-adapter"]
"#,
        ),
        Language::Bash => format!(
            r#"# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for {tool_id} (bash source as tool logic).

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates jq python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/tool
COPY tool-server /opt/tool/tool-server
COPY adapter/tool-adapter /tool-adapter
COPY adapter/sidecars/etc/agentium/tool-bundle.json /etc/agentium/tool-bundle.json

RUN chmod +x /tool-adapter /opt/tool/tool-server

ENTRYPOINT ["/tool-adapter"]
"#,
        ),
        Language::Rust => format!(
            r#"# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for {tool_id} (rust source as tool logic).

FROM rust:slim AS builder
WORKDIR /build
COPY Cargo.toml /build/Cargo.toml
COPY src /build/src
RUN cargo build --release --manifest-path /build/Cargo.toml

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/tool
COPY --from=builder /build/target/release/external-tool /opt/tool/external-tool
COPY adapter/tool-adapter /tool-adapter
COPY adapter/sidecars/etc/agentium/tool-bundle.json /etc/agentium/tool-bundle.json

RUN chmod +x /tool-adapter /opt/tool/external-tool

ENTRYPOINT ["/tool-adapter"]
"#,
        ),
        Language::Typescript => format!(
            r#"# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for {tool_id} (typescript source as tool logic).

FROM node:22-slim AS builder
WORKDIR /build
COPY package.json /build/package.json
COPY tsconfig.json /build/tsconfig.json
COPY src /build/src
RUN npm install && npm run build

FROM node:22-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/tool
COPY --from=builder /build/dist /opt/tool/dist
COPY adapter/tool-adapter /tool-adapter
COPY adapter/sidecars/etc/agentium/tool-bundle.json /etc/agentium/tool-bundle.json

RUN chmod +x /tool-adapter

ENTRYPOINT ["/tool-adapter"]
"#,
        ),
    }
}

fn adapter_script(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env python3
"""Generic TSRPC adapter shim.

Reads one framed JSON-RPC request from stdin and returns one framed response.

All methods (`tool/describe`, `tool/schema`, `tool/invoke`) are delegated to the
child command declared in `/etc/agentium/tool-bundle.json` (`runtime.command`).
The child tool is the sole source of truth for its own schema; this shim never
reads or serves a schema from the sidecar bundle.
"""

import json
import os
import stat
import struct
import subprocess
import sys

TOOL_ID = {tool_id:?}
METHOD_DESCRIBE = {method_describe:?}
METHOD_SCHEMA = {method_schema:?}
METHOD_INVOKE = {method_invoke:?}
SUPPORTED_METHODS = [METHOD_DESCRIBE, METHOD_SCHEMA, METHOD_INVOKE]
SIDECAR_BUNDLE = "/etc/agentium/tool-bundle.json"
MAX_SIDECAR_BYTES = 1_048_576
MAX_STATIC_RESPONSE_BYTES = 1_048_576
DEFAULT_SCHEMA_CONTENT_TYPE = "application/schema+json"
ERR_METHOD_NOT_FOUND = {err_method_not_found}
ERR_INVALID_PARAMS = {err_invalid_params}
ERR_SIDECAR_MISSING = {err_sidecar_missing}
ERR_SIDECAR_MALFORMED = {err_sidecar_malformed}
ERR_SIDECAR_SCHEMA_INVALID = {err_sidecar_schema_invalid}
ERR_SCHEMA_DIGEST_MISMATCH = {err_schema_digest_mismatch}
ERR_UNSUPPORTED_PROTOCOL = {err_unsupported_protocol}
ERR_PAYLOAD_LIMIT_EXCEEDED = {err_payload_limit_exceeded}
ERR_SIDECAR_SIZE_EXCEEDED = {err_sidecar_size_exceeded}


class SidecarError(Exception):
    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code


def fail(msg: str) -> None:
    print(f"{{TOOL_ID}} adapter error: {{msg}}", file=sys.stderr)
    raise SystemExit(1)


def read_exact(n: int) -> bytes:
    chunk = sys.stdin.buffer.read(n)
    if len(chunk) != n:
        fail(f"short read: expected {{n}} bytes, got {{len(chunk)}}")
    return chunk


def _reject_duplicate_keys(pairs):
    obj = {{}}
    for key, value in pairs:
        if key in obj:
            raise ValueError(f"duplicate JSON key: {{key}}")
        obj[key] = value
    return obj


def load_json_file(path: str):
    try:
        st = os.stat(path, follow_symlinks=False)
    except FileNotFoundError as exc:
        raise SidecarError(ERR_SIDECAR_MISSING, f"missing required sidecar bundle at {{path}}") from exc

    if stat.S_ISLNK(st.st_mode):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, f"sidecar path must not be a symlink: {{path}}")
    if not stat.S_ISREG(st.st_mode):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, f"sidecar path must be a regular file: {{path}}")

    try:
        with open(path, "rb") as f:
            raw = f.read(MAX_SIDECAR_BYTES + 1)
    except FileNotFoundError as exc:
        raise SidecarError(ERR_SIDECAR_MISSING, f"missing required sidecar bundle at {{path}}") from exc

    if len(raw) > MAX_SIDECAR_BYTES:
        raise SidecarError(
            ERR_SIDECAR_SIZE_EXCEEDED,
            f"sidecar size exceeded at {{path}} (max={{MAX_SIDECAR_BYTES}} bytes)",
        )

    if raw.startswith(b"\xef\xbb\xbf"):
        raise SidecarError(ERR_SIDECAR_MALFORMED, f"sidecar UTF-8 BOM not allowed at {{path}}")

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SidecarError(ERR_SIDECAR_MALFORMED, f"sidecar is not valid UTF-8 at {{path}}") from exc

    try:
        return json.loads(text, object_pairs_hook=_reject_duplicate_keys)
    except Exception as exc:  # noqa: BLE001
        raise SidecarError(ERR_SIDECAR_MALFORMED, f"sidecar malformed JSON at {{path}}: {{exc}}") from exc


def load_sidecar_bundle() -> dict:
    bundle = load_json_file(SIDECAR_BUNDLE)
    if not isinstance(bundle, dict):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "sidecar bundle must be a JSON object")
    return bundle


def load_runtime_spec(bundle: dict) -> dict:
    spec = bundle.get("runtime") if isinstance(bundle, dict) else None
    if not isinstance(spec, dict):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "sidecar bundle missing runtime object")

    command = spec.get("command")
    if not isinstance(command, list) or not command or not all(isinstance(x, str) and x for x in command):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "runtime sidecar missing valid command[]")

    protocol = spec.get("protocol")
    if protocol != "jsonrpc-stdio":
        raise SidecarError(ERR_UNSUPPORTED_PROTOCOL, f"unsupported protocol in runtime sidecar: {{protocol!r}}")

    return spec


def error_response(req_id, code: int, message: str) -> dict:
    return dict(
        jsonrpc="2.0",
        id=req_id,
        error=dict(code=code, message=message),
    )


def write_framed_response(response: dict, enforce_static_limit: bool = False) -> bool:
    response_body = json.dumps(response, separators=(",", ":")).encode("utf-8")
    if enforce_static_limit and len(response_body) > MAX_STATIC_RESPONSE_BYTES:
        return False
    sys.stdout.buffer.write(struct.pack(">I", len(response_body)))
    sys.stdout.buffer.write(response_body)
    sys.stdout.buffer.flush()
    return True


def load_runtime() -> dict:
    """Load and validate the child command from the sidecar bundle."""
    bundle = load_sidecar_bundle()
    return load_runtime_spec(bundle)


def forward_request(req: dict, runtime_spec: dict) -> dict:
    """Delegate a request to the child tool and return its JSON response.

    The child is the single source of truth for `tool/describe`, `tool/schema`,
    and `tool/invoke`. This shim only frames I/O and launches the child.
    """
    raw_req = (json.dumps(req, separators=(",", ":")) + "\n").encode("utf-8")
    proc = subprocess.run(
        runtime_spec["command"],
        input=raw_req,
        stdout=subprocess.PIPE,
        stderr=None,
        cwd=runtime_spec.get("workdir") or None,
        shell=False,
        check=False,
    )

    raw_out = proc.stdout.decode("utf-8", errors="replace").strip()
    if not raw_out:
        fail("tool process returned empty stdout")

    try:
        return json.loads(raw_out)
    except Exception as exc:  # noqa: BLE001
        fail(f"tool process returned invalid JSON: {{exc}}")


def main() -> int:
    hdr = sys.stdin.buffer.read(4)
    if len(hdr) != 4:
        fail("missing 4-byte frame header")

    (size,) = struct.unpack(">I", hdr)
    body = read_exact(size)

    try:
        req = json.loads(body.decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        write_framed_response(
            error_response(0, ERR_INVALID_PARAMS, f"invalid framed JSON request: {{exc}}")
        )
        return 0

    if not isinstance(req, dict):
        write_framed_response(error_response(0, ERR_INVALID_PARAMS, "request must be a JSON object"))
        return 0

    method = req.get("method")
    req_id = req.get("id")
    if not isinstance(method, str):
        write_framed_response(error_response(req_id or 0, ERR_INVALID_PARAMS, "missing/invalid method"))
        return 0

    try:
        runtime_spec = load_runtime()
    except SidecarError as exc:
        write_framed_response(error_response(req_id or 0, exc.code, str(exc)))
        return 0

    if method not in SUPPORTED_METHODS:
        write_framed_response(error_response(req_id, ERR_METHOD_NOT_FOUND, f"unknown method '{{method}}'"))
        return 0

    if method == METHOD_INVOKE:
        params = req.get("params")
        if params is not None and not isinstance(params, dict):
            write_framed_response(error_response(req_id, ERR_INVALID_PARAMS, "invalid params: expected object"))
            return 0

    response = forward_request(req, runtime_spec)
    write_framed_response(response, enforce_static_limit=False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#,
        tool_id = ctx.tool_id(),
        method_describe = METHOD_DESCRIBE,
        method_schema = METHOD_SCHEMA,
        method_invoke = METHOD_INVOKE,
        err_method_not_found = ERR_METHOD_NOT_FOUND,
        err_invalid_params = ERR_INVALID_PARAMS,
        err_sidecar_missing = ERR_SIDECAR_MISSING,
        err_sidecar_malformed = ERR_SIDECAR_MALFORMED,
        err_sidecar_schema_invalid = ERR_SIDECAR_SCHEMA_INVALID,
        err_schema_digest_mismatch = ERR_SCHEMA_DIGEST_MISMATCH,
        err_unsupported_protocol = ERR_UNSUPPORTED_PROTOCOL,
        err_payload_limit_exceeded = ERR_PAYLOAD_LIMIT_EXCEEDED,
        err_sidecar_size_exceeded = ERR_SIDECAR_SIZE_EXCEEDED,
    )
}

/// Starter echo-tool input/output JSON schemas.
///
/// Single source of truth shared by the sidecar bundle and every language
/// starter's `tool/schema` handler — keeps the declared contract and the
/// served schema in lockstep.
pub(crate) fn starter_schemas() -> baml_rt_tools::external_tools::MetadataSchemas {
    baml_rt_tools::external_tools::MetadataSchemas {
        input: serde_json::json!({
            "type": "object",
            "properties": { STARTER_INPUT_KEY: { "type": "string" } },
            "required": [STARTER_INPUT_KEY],
            "additionalProperties": false
        }),
        output: serde_json::json!({
            "type": "object",
            "properties": { STARTER_OUTPUT_KEY: { "type": "string" } },
            "required": [STARTER_OUTPUT_KEY],
            "additionalProperties": false
        }),
        events: Vec::new(),
    }
}

/// `(input_compact_json, output_compact_json, content_digest)` for the starter
/// schema.
///
/// The digest is computed with the SAME function the runner uses at discovery
/// ([`compute_external_schema_digest`]), so a freshly scaffolded tool's
/// self-reported `tool/schema` digest matches the runner's recomputation by
/// construction — no per-language JCS reimplementation required.
///
/// [`compute_external_schema_digest`]: baml_rt_tools::external_tools::compute_external_schema_digest
pub(crate) fn starter_schema_parts(ctx: &ScaffoldContext<'_>) -> (String, String, String) {
    let schemas = starter_schemas();
    let input = serde_json::to_string(&schemas.input).expect("serialize starter input schema");
    let output = serde_json::to_string(&schemas.output).expect("serialize starter output schema");
    let meta = manifest_json::build_manifest(ctx).into_metadata(schemas);
    let digest = baml_rt_tools::external_tools::compute_external_schema_digest(&meta).to_string();
    (input, output, digest)
}

fn scaffold_sidecar_bundle(ctx: &ScaffoldContext<'_>) -> String {
    let meta = manifest_json::build_manifest(ctx).into_metadata(starter_schemas());
    let bundle =
        render_sidecar_bundle(&meta).expect("render sidecar bundle from scaffold manifest");
    serde_json::to_string_pretty(&bundle).expect("serialize scaffold sidecar bundle") + "\n"
}

fn default_image_tag(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-sandbox:local", ctx.bundle, safe_name)
}

fn default_rootfs_dir(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-rootfs", ctx.bundle, safe_name)
}
