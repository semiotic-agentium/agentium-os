#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Dev: commit+push deploy refs. Semver: verify + git tag push only.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
  echo "publish-deploy-release: $*" >&2
  exit 1
}

require_cmd git

if [[ "$#" -eq 0 ]]; then
  GIT_REF="main"
  IMG_TAG="latest"
elif [[ "$#" -eq 1 ]]; then
  GIT_REF="$1"
  if [[ "${GIT_REF}" == v* ]] && [[ "${GIT_REF}" != */* ]]; then
    IMG_TAG="${GIT_REF}"
  else
    IMG_TAG="latest"
  fi
else
  GIT_REF="$1"
  IMG_TAG="$2"
fi

_want_git_tag() {
  [[ "${GIT_REF}" == v* ]] && [[ "${GIT_REF}" != */* ]]
}

DRY="${AGENTIUM_RELEASE_DRY_RUN:-0}"
TRACK="${ROOT}/deploy/argocd/track.json"
IMAGES="${ROOT}/deploy/values/dev/images.yaml"

if [[ "$DRY" == "1" ]]; then
  echo "publish-deploy-release: DRY-RUN ${GIT_REF} (image ${IMG_TAG})" >&2
  exit 0
fi

cd "${ROOT}"

if _want_git_tag; then
  export GITHUB_REF_NAME="${GIT_REF}"
  bash "${ROOT}/scripts/release/verify-release-tag-matches-workspace-version.sh" \
    || die "tag ${GIT_REF} must match Cargo.toml workspace version"
  if git rev-parse "${GIT_REF}" >/dev/null 2>&1; then
    if [[ "${AGENTIUM_RELEASE_RETAG:-0}" == "1" ]]; then
      git tag -d "${GIT_REF}"
      git push origin ":refs/tags/${GIT_REF}" 2>/dev/null || true
    else
      die "tag ${GIT_REF} already exists (use AGENTIUM_RELEASE_RETAG=1 to replace)"
    fi
  fi
  git tag -a "${GIT_REF}" -m "Release ${GIT_REF}"
  branch="$(git rev-parse --abbrev-ref HEAD)"
  git push origin "refs/heads/${branch}:refs/heads/${branch}"
  git push origin "refs/tags/${GIT_REF}:refs/tags/${GIT_REF}"
  echo "publish-deploy-release: pushed tag ${GIT_REF} (deploy refs updated after images exist)"
else
  bash "${ROOT}/scripts/release/set-deploy-ref.sh" "${GIT_REF}" "${IMG_TAG}"
  git add "${TRACK}" "${IMAGES}"
  if git diff --cached --quiet; then
    echo "publish-deploy-release: no deploy ref changes"
    exit 0
  fi
  git commit -m "release: ${GIT_REF} (${IMG_TAG})"
  branch="$(git rev-parse --abbrev-ref HEAD)"
  git push origin "refs/heads/${branch}:refs/heads/${branch}"
  echo "publish-deploy-release: pushed deploy refs for ${GIT_REF} + ${IMG_TAG}"
fi
