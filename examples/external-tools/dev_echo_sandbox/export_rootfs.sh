#!/usr/bin/env bash
set -euo pipefail

# Export a Docker image filesystem into a directory usable as microsandbox bind rootfs.
#
# Usage:
#   ./examples/external-tools/dev_echo_sandbox/export_rootfs.sh \
#     --image dev-echo-sandbox:local \
#     --out ./.tmp/dev-echo-rootfs

IMAGE="dev-echo-sandbox:local"
OUT="${PWD}/.tmp/dev-echo-rootfs"
FORCE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image)
      IMAGE="$2"
      shift 2
      ;;
    --out)
      OUT="$2"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    -h|--help)
      cat <<'EOF'
Export image rootfs to a directory.

Options:
  --image <name:tag>   Docker image (default: dev-echo-sandbox:local)
  --out <dir>          Output directory (default: $PWD/.tmp/dev-echo-rootfs)
  --force              Remove output directory if it exists
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

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "Image not found: $IMAGE" >&2
  exit 1
fi

if [[ -e "$OUT" && $FORCE -eq 1 ]]; then
  rm -rf "$OUT"
fi

mkdir -p "$OUT"

CID=""
cleanup() {
  if [[ -n "$CID" ]]; then
    docker rm "$CID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

CID=$(docker create "$IMAGE")
docker export "$CID" | tar -x -C "$OUT"

echo "Rootfs exported"
echo "  image:  $IMAGE"
echo "  out:    $OUT"
echo
echo "Quick check:"
ls -la "$OUT" | head -n 20
