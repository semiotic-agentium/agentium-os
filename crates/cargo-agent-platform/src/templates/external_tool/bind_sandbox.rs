//! Shared scaffold helpers for sandbox bind setup scripts.
//!
//! Docker-assisted artifacts are optional and only emitted when
//! `new-tool --runtime sandbox --sandbox-source bind --generate-docker`.

use super::{GeneratedFile, Language, ScaffoldContext};
pub fn files(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    if !ctx.generate_docker {
        return Vec::new();
    }

    vec![
        GeneratedFile::new("setup_bind_sandbox.sh", setup_script_with_docker(ctx)).executable(),
        GeneratedFile::new("adapter/Dockerfile", dockerfile(ctx)),
        GeneratedFile::new("adapter/tool-adapter", adapter_script(ctx)).executable(),
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
#   4) patches tool-metadata.json with bind path + digest
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

echo "Bind metadata patched and validated."
echo "  tool:           {tool_id}"
echo "  image:          $IMAGE"
echo "  bind path:      $ROOTFS"
"#,
        default_image = default_image_tag(ctx),
        rootfs_dir = default_rootfs_dir(ctx),
        tool_id = ctx.tool_id(),
    )
}

fn dockerfile(ctx: &ScaffoldContext<'_>) -> String {
    let tool_id = ctx.tool_id();
    let tool_cmd = tool_cmd(ctx);

    match ctx.language {
        Language::Python => format!(
            r#"# syntax=docker/dockerfile:1.7

# Sandbox adapter image scaffold for {tool_id} (python source as tool logic).

FROM python:3.12-slim

WORKDIR /opt/tool
COPY main.py /opt/tool/main.py
COPY adapter/tool-adapter /tool-adapter

RUN chmod +x /tool-adapter /opt/tool/main.py

ENV TOOL_CMD={tool_cmd}
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

RUN chmod +x /tool-adapter /opt/tool/tool-server

ENV TOOL_CMD={tool_cmd}
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

RUN chmod +x /tool-adapter /opt/tool/external-tool

ENV TOOL_CMD={tool_cmd}
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

RUN chmod +x /tool-adapter

ENV TOOL_CMD={tool_cmd}
ENTRYPOINT ["/tool-adapter"]
"#,
        ),
    }
}

fn adapter_script(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env python3
"""Generic TSRPC adapter shim.

Reads one framed JSON-RPC request from stdin, delegates to TOOL_CMD using raw
JSON-RPC over stdio, then returns one framed response.

This keeps scaffolded language sources simple while still matching sandbox
transport requirements.
"""

import json
import os
import struct
import subprocess
import sys

TOOL_ID = {tool_id:?}


def fail(msg: str) -> None:
    print(f"{{TOOL_ID}} adapter error: {{msg}}", file=sys.stderr)
    raise SystemExit(1)


def read_exact(n: int) -> bytes:
    chunk = sys.stdin.buffer.read(n)
    if len(chunk) != n:
        fail(f"short read: expected {{n}} bytes, got {{len(chunk)}}")
    return chunk


def main() -> int:
    tool_cmd = os.environ.get("TOOL_CMD", "").strip()
    if not tool_cmd:
        fail("TOOL_CMD env var is required")

    hdr = sys.stdin.buffer.read(4)
    if len(hdr) != 4:
        fail("missing 4-byte frame header")

    (size,) = struct.unpack(">I", hdr)
    body = read_exact(size)

    try:
        req = json.loads(body.decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        fail(f"invalid framed JSON request: {{exc}}")

    raw_req = (json.dumps(req, separators=(",", ":")) + "\n").encode("utf-8")

    proc = subprocess.run(
        tool_cmd,
        input=raw_req,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        shell=True,
        check=False,
    )

    if proc.stderr:
        sys.stderr.write(proc.stderr.decode("utf-8", errors="replace"))

    raw_out = proc.stdout.decode("utf-8", errors="replace").strip()
    if not raw_out:
        fail("tool process returned empty stdout")

    try:
        response = json.loads(raw_out)
    except Exception as exc:  # noqa: BLE001
        fail(f"tool process returned invalid JSON: {{exc}}")

    response_body = json.dumps(response, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(response_body)))
    sys.stdout.buffer.write(response_body)
    sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
"#,
        tool_id = ctx.tool_id(),
    )
}

fn tool_cmd(ctx: &ScaffoldContext<'_>) -> &'static str {
    match ctx.language {
        Language::Rust => "\"/opt/tool/external-tool\"",
        Language::Bash => "\"/opt/tool/tool-server\"",
        Language::Python => "\"python3 /opt/tool/main.py\"",
        Language::Typescript => "\"node /opt/tool/dist/main.js\"",
    }
}

fn default_image_tag(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-sandbox:local", ctx.bundle, safe_name)
}

fn default_rootfs_dir(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-rootfs", ctx.bundle, safe_name)
}
