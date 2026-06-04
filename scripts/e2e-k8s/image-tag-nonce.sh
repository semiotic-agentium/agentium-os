#!/usr/bin/env bash
# Generate a fresh local image tag nonce and persist for build/render scripts.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

GENERATED_DIR="${ROOT}/deploy/values/generated"
LAST_TAG_FILE="${GENERATED_DIR}/.last-image-tag"
mkdir -p "${GENERATED_DIR}"

if [[ -n "${AGENTIUM_FIXED_TAG:-}" ]]; then
  TAG="${AGENTIUM_FIXED_TAG}"
else
  nonce="$(uuidgen 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -d '-' | cut -c1-6 || true)"
  if [[ -z "${nonce}" ]]; then
    nonce="$(printf '%06x' "$((RANDOM<<8 | RANDOM))" | cut -c1-6)"
  fi
  TAG="local-dev-$(date +%Y%m%d%H%M%S)-${nonce}"
fi
printf '%s\n' "${TAG}" >"${LAST_TAG_FILE}"
echo "image-tag-nonce: wrote ${LAST_TAG_FILE} (${TAG})"
