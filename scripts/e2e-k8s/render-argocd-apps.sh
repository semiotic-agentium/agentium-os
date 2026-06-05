#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Renders Argo CD Application manifests from deploy/argocd/track.json.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || {
    echo "render-argocd-apps: required command not found: ${cmd}" >&2
    exit 1
  }
}

die() {
  echo "render-argocd-apps: $*" >&2
  exit 1
}

require_cmd python3

TRACK="${ROOT}/deploy/argocd/track.json"
[[ -f "${TRACK}" ]] || die "missing ${TRACK}"

_track_repo() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["repoURL"])' "${TRACK}"
}
_track_rev() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["targetRevision"])' "${TRACK}"
}

REPO="${AGENTIUM_GIT_REPO:-$(_track_repo)}"
REV="${AGENTIUM_GIT_REVISION:-$(_track_rev)}"
OUT_DIR="${ROOT}/deploy/argocd/.rendered"
APP_DIR="${ROOT}/deploy/argocd/apps"
mkdir -p "${OUT_DIR}"

_render() {
  local infile="$1"
  local base
  base="$(basename "${infile}")"
  sed -e "s|__AGENTIUM_GIT_REPO__|${REPO}|g" -e "s|__AGENTIUM_GIT_REVISION__|${REV}|g" "${infile}" \
    >"${OUT_DIR}/${base}"
}

_render "${APP_DIR}/root-application.yaml"
cp "${APP_DIR}/project.yaml" "${OUT_DIR}/project.yaml"

if grep -q '__AGENTIUM_GIT_REPO__\|__AGENTIUM_GIT_REVISION__' "${OUT_DIR}/root-application.yaml" 2>/dev/null; then
  die "${OUT_DIR}/root-application.yaml still contains __AGENTIUM_* placeholders"
fi

echo "render-argocd-apps: wrote ${OUT_DIR} repo=${REPO} rev=${REV}"
