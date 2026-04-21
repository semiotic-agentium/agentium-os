#!/usr/bin/env bash
set -euo pipefail

# One-shot local bind demo setup for examples/external-tools/dev_echo_sandbox.
#
# What it does:
#  1) exports Docker image rootfs into a bind directory
#  2) computes bind runtime_digest via `cargo agent-platform sandbox-digest`
#  3) patches dev_echo_sandbox/tool-metadata.json to bind mode
#  4) validates metadata with check-external-tool
#  5) prints env vars to run the runner
#
# Usage:
#   ./examples/external-tools/dev_echo_sandbox/setup_bind_demo.sh
#   ./examples/external-tools/dev_echo_sandbox/setup_bind_demo.sh --image dev-echo-sandbox:local --force

IMAGE="dev-echo-sandbox:local"
FORCE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image)
      IMAGE="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    -h|--help)
      cat <<'EOF'
setup_bind_demo.sh

Options:
  --image <name:tag>   Docker image to export (default: dev-echo-sandbox:local)
  --force              Re-export rootfs even if output dir exists
  -h, --help           Show this help
EOF
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
EXAMPLE_DIR="$REPO_ROOT/examples/external-tools/dev_echo_sandbox"
ROOTFS_DIR="$REPO_ROOT/.tmp/dev-echo-rootfs"
TOOL_METADATA="$EXAMPLE_DIR/tool-metadata.json"

mkdir -p "$REPO_ROOT/.tmp"

if [[ $FORCE -eq 1 || ! -d "$ROOTFS_DIR" || -z "$(ls -A "$ROOTFS_DIR" 2>/dev/null || true)" ]]; then
  "$EXAMPLE_DIR/export_rootfs.sh" --image "$IMAGE" --out "$ROOTFS_DIR" ${FORCE:+--force}
else
  echo "Reusing existing rootfs: $ROOTFS_DIR"
fi

DIGEST="$(cargo run -q -p cargo-agent-platform -- sandbox-digest --source bind "$ROOTFS_DIR")"
if [[ -z "$DIGEST" ]]; then
  echo "Failed to compute runtime_digest" >&2
  exit 1
fi

jq --arg path "$ROOTFS_DIR" --arg digest "$DIGEST" '
  .runtime.image = {"kind":"bind","path":$path}
  | .runtime.entrypoint = ["/tool-adapter"]
  | .runtime_digest = $digest
' "$TOOL_METADATA" > /tmp/dev-echo-tool-metadata.json
mv /tmp/dev-echo-tool-metadata.json "$TOOL_METADATA"

echo "Patched metadata: $TOOL_METADATA"
echo "  bind path:      $ROOTFS_DIR"
echo "  runtime_digest: $DIGEST"
echo ""
echo "NOTE: this script updates tracked example metadata for local execution."
echo "      Reset before committing with:"
echo "      git checkout -- examples/external-tools/dev_echo_sandbox/tool-metadata.json"

cargo run -q -p cargo-agent-platform -- check-external-tool --path "$EXAMPLE_DIR"

echo
echo "Setup complete. Export these vars before running the runner:"
echo "  export BAML_EXTERNAL_TOOLS_DIR=\"$EXAMPLE_DIR\""
echo "  export BAML_SANDBOX_PROVIDER=microsandbox"
echo "  export BAML_SANDBOX_BIND_ROOTS=\"$REPO_ROOT/.tmp\""
