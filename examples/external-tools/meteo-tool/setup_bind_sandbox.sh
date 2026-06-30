#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Bind sandbox setup helper (Docker-assisted mode).
#
# This script delegates to `sandbox-bind-sync`, which:
#   1) builds adapter image from adapter/Dockerfile
#   2) exports image filesystem into a bind rootfs directory
#   3) writes tool-manifest.lock.json with the local bind path
#   4) writes the sidecar bundle into the rootfs
#   5) validates metadata via `check-external-tool` (with --check)

run_agent_platform() {
  local subcmd="${1:-}"
  shift || true

  if [[ -n "${AGENT_PLATFORM_CMD:-}" ]]; then
    # shellcheck disable=SC2206
    local cmd=( $AGENT_PLATFORM_CMD )
    "${cmd[@]}" "$subcmd" "$@"
    return
  fi

  if agentium "$subcmd" --help >/dev/null 2>&1; then
    agentium "$subcmd" "$@"
    return
  fi

  if cargo run -q -p agentium -- "$subcmd" --help >/dev/null 2>&1; then
    cargo run -q -p agentium -- "$subcmd" "$@"
    return
  fi

  cat >&2 <<'EOF'
Could not find a compatible agentium command.

Tried:
  1) agentium <subcommand>
  2) cargo run -q -p agentium -- <subcommand>

You can override command resolution with:
  export AGENT_PLATFORM_CMD='cargo run -q -p agentium --'
(or another explicit command that supports sandbox-bind-sync)
EOF
  exit 1
}

IMAGE="dev-meteo-tool-sandbox:local"
ROOTFS=""
FORCE=0

usage() {
  cat <<EOF
setup_bind_sandbox.sh

Usage:
  ./setup_bind_sandbox.sh [--image name:tag] [--rootfs /abs/path|rel/path] [--force]

Options:
  --image <name:tag>   Docker image tag to build/export (default: dev-meteo-tool-sandbox:local)
  --rootfs <dir>       Bind rootfs output directory
                       (default: <tool-dir>/.tmp/dev-meteo-tool-rootfs)
  --force              Recreate rootfs output directory before export
  -h, --help           Show this help
EOF
}

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
  ROOTFS="$TOOL_DIR/.tmp/dev-meteo-tool-rootfs"
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

run_agent_platform "${args[@]}"

echo "Bind metadata patched and validated."
echo "  tool:           dev/meteo-tool"
echo "  image:          $IMAGE"
echo "  bind path:      $ROOTFS"
