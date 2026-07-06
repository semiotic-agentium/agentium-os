#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Fail if release tag vX.Y.Z does not match [workspace.package].version in Cargo.toml.
set -euo pipefail

# GitHub Actions does not allow overriding GITHUB_* defaults. RELEASE_TAG lets
# workflows that run on main (for example release-please) verify a newly-created
# tag without accidentally reading GITHUB_REF_NAME=main.
ref="${RELEASE_TAG:-${GITHUB_REF_NAME:-${CIRCLE_TAG:-}}}"
if [[ -z "$ref" ]]; then
  echo "verify-release-tag: set RELEASE_TAG, GITHUB_REF_NAME, or CIRCLE_TAG (e.g. v0.2.0)" >&2
  exit 2
fi

if [[ "$ref" != v* ]]; then
  echo "verify-release-tag: expected tag vX.Y.Z, got ${ref}" >&2
  exit 2
fi

ver="${ref#v}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cargo_toml="${root}/Cargo.toml"

parsed="$(bash "${root}/scripts/release/workspace-version.sh")"

if [[ -z "$parsed" ]]; then
  echo "verify-release-tag: could not read workspace version from ${cargo_toml}" >&2
  exit 2
fi

if [[ "$parsed" != "$ver" ]]; then
  echo "verify-release-tag: tag ${ref} expects ${ver}, Cargo.toml has ${parsed}" >&2
  exit 1
fi

chart_app="$(awk '/^appVersion:/ { gsub(/"/, "", $2); print $2; exit }' "${root}/deploy/helm/agentium-os/Chart.yaml")"
if [[ -n "$chart_app" && "$chart_app" != "$ver" ]]; then
  echo "verify-release-tag: Chart.yaml appVersion ${chart_app} != workspace ${ver}" >&2
  exit 1
fi

echo "verify-release-tag: OK tag ${ref} matches workspace version ${parsed}"
