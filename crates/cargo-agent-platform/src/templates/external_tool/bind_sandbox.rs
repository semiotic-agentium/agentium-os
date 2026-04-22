//! Shared scaffold helpers for sandbox bind setup scripts.
//!
//! Docker-assisted artifacts are optional and only emitted when
//! `new-tool --runtime sandbox --sandbox-source bind --generate-docker`.

use super::{GeneratedFile, STARTER_INPUT_KEY, ScaffoldContext};

pub fn files(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    if !ctx.generate_docker {
        return Vec::new();
    }

    vec![
        GeneratedFile::new("setup_bind_sandbox.sh", setup_script_with_docker(ctx)).executable(),
        GeneratedFile::new("adapter/Dockerfile", dockerfile(ctx)),
        GeneratedFile::new("adapter/tool-adapter", adapter_script(ctx)).executable(),
        GeneratedFile::new("inspect_tsrpc.py", inspect_tsrpc(ctx)).executable(),
    ]
}

fn setup_script_with_docker(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

# Bind sandbox setup helper (Docker-assisted mode).
#
# This script:
#   1) builds adapter image from adapter/Dockerfile
#   2) exports image filesystem into a bind rootfs directory
#   3) computes runtime_digest via `sandbox-digest`
#   4) patches tool-metadata.json with bind path + digest
#   5) validates metadata via `check-external-tool`

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
(or another explicit command that supports sandbox-digest/check-external-tool)
EOF
  exit 1
}}

IMAGE="{default_image}"
ROOTFS="$(pwd)/.tmp/{rootfs_dir}"
FORCE=0

usage() {{
  cat <<'EOF'
setup_bind_sandbox.sh

Usage:
  ./setup_bind_sandbox.sh [--image name:tag] [--rootfs /abs/path] [--force]

Options:
  --image <name:tag>   Docker image tag to build/export (default: {default_image})
  --rootfs <dir>       Bind rootfs output directory (default: $(pwd)/.tmp/{rootfs_dir})
  --force              Remove rootfs output directory before export
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
TOOL_METADATA="$TOOL_DIR/tool-metadata.json"
DOCKERFILE="$TOOL_DIR/adapter/Dockerfile"

if [[ ! -f "$DOCKERFILE" ]]; then
  echo "Missing adapter Dockerfile: $DOCKERFILE" >&2
  exit 1
fi

mkdir -p "$(dirname "$ROOTFS")"

echo "Building Docker image: $IMAGE"
docker build -t "$IMAGE" -f "$DOCKERFILE" "$TOOL_DIR"

if [[ -e "$ROOTFS" && $FORCE -eq 1 ]]; then
  rm -rf "$ROOTFS"
fi
mkdir -p "$ROOTFS"

CID=""
cleanup() {{
  if [[ -n "$CID" ]]; then
    docker rm "$CID" >/dev/null 2>&1 || true
  fi
}}
trap cleanup EXIT

CID=$(docker create "$IMAGE")
docker export "$CID" | tar -x -C "$ROOTFS"

DIGEST="$(run_agent_platform sandbox-digest --source bind "$ROOTFS")"
if [[ -z "$DIGEST" ]]; then
  echo "Failed to compute runtime_digest" >&2
  exit 1
fi

TMP_META="$(mktemp)"
jq --arg path "$ROOTFS" --arg digest "$DIGEST" '
  .runtime.image = {{"kind":"bind","path":$path}}
  | .runtime_digest = $digest
' "$TOOL_METADATA" > "$TMP_META"
mv "$TMP_META" "$TOOL_METADATA"

run_agent_platform check-external-tool --path "$TOOL_DIR"

echo "Bind metadata patched and validated."
echo "  tool:           {tool_id}"
echo "  image:          $IMAGE"
echo "  bind path:      $ROOTFS"
echo "  runtime_digest: $DIGEST"
"#,
        default_image = default_image_tag(ctx),
        rootfs_dir = default_rootfs_dir(ctx),
        tool_id = ctx.tool_id(),
    )
}

fn dockerfile(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"# syntax=docker/dockerfile:1.7

# Starter adapter image scaffold for {tool_id}.
#
# Replace adapter/tool-adapter with your production sandbox adapter binary/script.
# The setup_bind_sandbox.sh helper will build this image and export the rootfs
# for bind-mode metadata patching.

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY adapter/tool-adapter /tool-adapter
RUN chmod +x /tool-adapter

ENTRYPOINT ["/tool-adapter"]
"#,
        tool_id = ctx.tool_id(),
    )
}

fn adapter_script(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

# TODO: replace this starter adapter with your sandbox implementation.
# This placeholder intentionally fails so unfinished images are obvious.

echo "{tool_id}: adapter placeholder reached; implement adapter/tool-adapter" >&2
exit 1
"#,
        tool_id = ctx.tool_id(),
    )
}

fn inspect_tsrpc(ctx: &ScaffoldContext<'_>) -> String {
    format!(
        r#"#!/usr/bin/env python3
"""Quick TSRPC inspector for sandbox adapter binaries.

Examples:
  # describe
  ./inspect_tsrpc.py --adapter ./.tmp/{rootfs_dir}/tool-adapter describe

  # invoke
  ./inspect_tsrpc.py --adapter ./.tmp/{rootfs_dir}/tool-adapter invoke --message "hello"
"""

from __future__ import annotations

import argparse
import json
import struct
import subprocess
import sys
from pathlib import Path


def send_frame(proc: subprocess.Popen[bytes], payload: dict) -> None:
    body = json.dumps(payload).encode("utf-8")
    proc.stdin.write(struct.pack(">I", len(body)))
    proc.stdin.write(body)
    proc.stdin.flush()


def recv_frame(proc: subprocess.Popen[bytes]) -> dict:
    hdr = proc.stdout.read(4)
    if len(hdr) < 4:
        raise RuntimeError("no frame header received from adapter")
    (size,) = struct.unpack(">I", hdr)
    data = proc.stdout.read(size)
    if len(data) < size:
        raise RuntimeError(f"short frame body: got {{len(data)}} expected {{size}}")
    return json.loads(data.decode("utf-8"))


def run(adapter: Path, request: dict, timeout_s: float) -> int:
    if not adapter.exists():
        print(f"adapter not found: {{adapter}}", file=sys.stderr)
        return 2

    proc = subprocess.Popen(
        [str(adapter)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        assert proc.stdin and proc.stdout and proc.stderr
        send_frame(proc, request)
        response = recv_frame(proc)
        print(json.dumps(response, indent=2))
        return 0
    except Exception as exc:
        print(f"request failed: {{exc}}", file=sys.stderr)
        try:
            err = proc.stderr.read().decode("utf-8", errors="replace")
            if err:
                print("--- adapter stderr ---", file=sys.stderr)
                print(err, file=sys.stderr)
        except Exception:
            pass
        return 1
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=timeout_s)
        except Exception:
            proc.kill()


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser()
    p.add_argument(
        "--adapter",
        default="./.tmp/{rootfs_dir}/tool-adapter",
        help="path to adapter binary",
    )
    p.add_argument("--timeout", type=float, default=3.0, help="process shutdown timeout")

    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("describe")

    inv = sub.add_parser("invoke")
    inv.add_argument("--message", default="hello", help="input.{input_key}")
    inv.add_argument("--tool-name", default="{tool_id}", help="params.tool_name")
    inv.add_argument("--invocation-id", default="manual-1", help="params.invocation_id")

    return p


def main() -> int:
    args = build_parser().parse_args()
    adapter = Path(args.adapter)

    if args.cmd == "describe":
        req = {{"jsonrpc": "2.0", "id": 1, "method": "tool/describe", "params": {{}}}}
    else:
        req = {{
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tool/invoke",
            "params": {{
                "invocation_id": args.invocation_id,
                "tool_name": args.tool_name,
                "input": {{"{input_key}": args.message}},
                "secrets": {{}},
                "capabilities": None,
            }},
        }}

    return run(adapter, req, args.timeout)


if __name__ == "__main__":
    raise SystemExit(main())
"#,
        rootfs_dir = default_rootfs_dir(ctx),
        tool_id = ctx.tool_id(),
        input_key = STARTER_INPUT_KEY,
    )
}

fn default_image_tag(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-sandbox:local", ctx.bundle, safe_name)
}

fn default_rootfs_dir(ctx: &ScaffoldContext<'_>) -> String {
    let safe_name = ctx.name.replace('_', "-");
    format!("{}-{}-rootfs", ctx.bundle, safe_name)
}
