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

use super::{GeneratedFile, Language, STARTER_INPUT_KEY, ScaffoldContext, metadata_json};
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
#   3) computes runtime_digest from rootfs contents
#   4) writes tool-metadata.lock.json (gitignored) with bind path + digest
#   5) validates metadata via `check-external-tool` (with --check)

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

For `describe` and `schema`, this script reads the bind runtime artifact:
  <rootfs>/etc/agentium/tool-bundle.json

For `invoke`, it sends framed TSRPC JSON-RPC to an adapter command you provide
after `--`.

Examples:
  ./inspect_tool.py describe
  ./inspect_tool.py schema --rootfs ./.tmp/__ROOTFS_DIR__
  ./inspect_tool.py invoke --input '{"__STARTER_INPUT_KEY__":"hello"}' -- docker run --rm -i __DEFAULT_IMAGE__ /tool-adapter
"""

import argparse
import json
import pathlib
import struct
import subprocess

TOOL_ID = __TOOL_ID__
DEFAULT_ROOTFS = pathlib.Path(__file__).resolve().parent / ".tmp" / __ROOTFS_DIR_PY__


def _read_bundle(rootfs: pathlib.Path) -> dict:
    bundle_path = rootfs / "etc" / "agentium" / "tool-bundle.json"
    if not bundle_path.exists():
        raise SystemExit(
            f"missing sidecar bundle at {bundle_path}\n"
            "Hint: run ./setup_bind_sandbox.sh --force or sandbox-bind-sync first."
        )
    try:
        return json.loads(bundle_path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(f"failed to parse {bundle_path}: {exc}")


def _describe_from_bundle(bundle: dict) -> dict:
    manifest = bundle.get("manifest") or {}
    schema = bundle.get("schema") or {}
    runtime = bundle.get("runtime") or {}
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocol_version": manifest.get("protocol_version", "2"),
            "tool_name": manifest.get("tool_name", runtime.get("tool_id", TOOL_ID)),
            "supported_methods": manifest.get("supported_methods", ["tool/describe", "tool/schema", "tool/invoke"]),
            "schema_digest": schema.get("content_digest"),
        },
    }


def _schema_from_bundle(bundle: dict) -> dict:
    schema = bundle.get("schema")
    if not isinstance(schema, dict):
        raise SystemExit("sidecar bundle missing schema object")
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "result": schema,
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
        "--rootfs",
        default=str(DEFAULT_ROOTFS),
        help="bind rootfs path for describe/schema artifact inspection",
    )
    parser.add_argument(
        "--input",
        default='{"__STARTER_INPUT_KEY__":"hello"}',
        help="invoke input JSON (used only for mode=invoke)",
    )
    parser.add_argument(
        "cmd",
        nargs=argparse.REMAINDER,
        help="adapter command for mode=invoke (pass after --)",
    )
    args = parser.parse_args()

    cmd = list(args.cmd or [])
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]

    return args, cmd


def main() -> int:
    args, cmd = _parse_args()

    if args.mode in ("describe", "schema"):
        bundle = _read_bundle(pathlib.Path(args.rootfs))
        resp = _describe_from_bundle(bundle) if args.mode == "describe" else _schema_from_bundle(bundle)
        print(json.dumps(resp, indent=2, ensure_ascii=False))
        return 0

    if not cmd:
        raise SystemExit("mode=invoke requires adapter command after '--' (e.g. docker run ... /tool-adapter)")

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

- `tool/describe` is handled directly in this adapter (manifest-aware)
- `tool/invoke` delegates to the child command declared in
  `/etc/agentium/tool-bundle.json` (`runtime.command`)
"""

import hashlib
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


def load_describe_manifest(bundle: dict, runtime_spec: dict) -> dict:
    manifest = bundle.get("manifest") if isinstance(bundle, dict) else None
    if not isinstance(manifest, dict):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "sidecar bundle missing manifest object")
    return manifest


def load_schema(bundle: dict, runtime_spec: dict) -> dict:
    schema = bundle.get("schema") if isinstance(bundle, dict) else None
    if not isinstance(schema, dict):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "sidecar bundle missing schema object")
    if schema.get("content_type") is None:
        schema["content_type"] = DEFAULT_SCHEMA_CONTENT_TYPE
    return schema


def compute_schema_digest(schema: dict) -> str:
    payload = dict(input=schema.get("input"), output=schema.get("output"))
    # NOTE: this is compact deterministic JSON canonicalization for startup
    # validation in the shim. Host-side digest generation is authoritative.
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return "sha256:" + hashlib.sha256(canonical).hexdigest()


def validate_bundle(bundle: dict, runtime_spec: dict, schema: dict) -> None:
    declared = schema.get("content_digest")
    if not isinstance(declared, str) or not declared.startswith("sha256:"):
        raise SidecarError(ERR_SIDECAR_SCHEMA_INVALID, "schema sidecar missing valid content_digest")

    computed = compute_schema_digest(schema)
    if computed != declared:
        raise SidecarError(
            ERR_SCHEMA_DIGEST_MISMATCH,
            f"schema digest mismatch: expected {{declared}} got {{computed}}",
        )

    manifest = bundle.get("manifest") if isinstance(bundle, dict) else None
    if isinstance(manifest, dict):
        methods = manifest.get("supported_methods")
        if not isinstance(methods, list) or METHOD_INVOKE not in methods:
            raise SidecarError(
                ERR_SIDECAR_SCHEMA_INVALID,
                f"manifest sidecar missing required supported_methods entry '{{METHOD_INVOKE}}'",
            )


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


def load_snapshot() -> tuple[dict, dict, dict]:
    """Load immutable startup snapshot from sidecar bundle."""
    bundle = load_sidecar_bundle()
    runtime_spec = load_runtime_spec(bundle)
    schema = load_schema(bundle, runtime_spec)
    manifest = load_describe_manifest(bundle, runtime_spec)
    validate_bundle(bundle, runtime_spec, schema)
    return runtime_spec, manifest, schema


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
        runtime_spec, manifest, schema = load_snapshot()
    except SidecarError as exc:
        write_framed_response(error_response(req_id or 0, exc.code, str(exc)))
        return 0

    if method == METHOD_DESCRIBE:
        response = dict(
            jsonrpc="2.0",
            id=req_id,
            result=dict(
                protocol_version=manifest.get("protocol_version", "2"),
                tool_name=manifest.get("tool_name", runtime_spec.get("tool_id", TOOL_ID)),
                supported_methods=manifest.get("supported_methods", SUPPORTED_METHODS),
                schema_digest=schema.get("content_digest"),
            ),
        )
        if not write_framed_response(response, enforce_static_limit=True):
            write_framed_response(
                error_response(req_id, ERR_PAYLOAD_LIMIT_EXCEEDED, "static response payload limit exceeded")
            )
        return 0

    if method == METHOD_SCHEMA:
        response = dict(jsonrpc="2.0", id=req_id, result=schema)
        if not write_framed_response(response, enforce_static_limit=True):
            write_framed_response(
                error_response(req_id, ERR_PAYLOAD_LIMIT_EXCEEDED, "static response payload limit exceeded")
            )
        return 0

    if method != METHOD_INVOKE:
        write_framed_response(
            error_response(req_id, ERR_METHOD_NOT_FOUND, f"unknown method '{{method}}'"),
            enforce_static_limit=False,
        )
        return 0

    params = req.get("params")
    if params is not None and not isinstance(params, dict):
        write_framed_response(error_response(req_id, ERR_INVALID_PARAMS, "invalid params: expected object"))
        return 0

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
        response = json.loads(raw_out)
    except Exception as exc:  # noqa: BLE001
        fail(f"tool process returned invalid JSON: {{exc}}")

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

fn scaffold_sidecar_bundle(ctx: &ScaffoldContext<'_>) -> String {
    let meta = metadata_json::build_metadata(ctx);
    let runtime_digest = meta
        .runtime_digest
        .as_deref()
        .unwrap_or("sha256:0000000000000000000000000000000000000000000000000000000000000000");
    let bundle = render_sidecar_bundle(&meta, runtime_digest)
        .expect("render sidecar bundle from scaffold metadata");
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
