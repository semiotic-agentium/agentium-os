#!/usr/bin/env bash
# Fail if release tag vX.Y.Z does not match [workspace.package].version in Cargo.toml.
set -euo pipefail

ref="${GITHUB_REF_NAME:-${CIRCLE_TAG:-}}"
if [[ -z "$ref" ]]; then
  echo "verify-release-tag: set GITHUB_REF_NAME or CIRCLE_TAG (e.g. v0.2.0)" >&2
  exit 2
fi

if [[ "$ref" != v* ]]; then
  echo "verify-release-tag: expected tag vX.Y.Z, got ${ref}" >&2
  exit 2
fi

ver="${ref#v}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cargo_toml="${root}/Cargo.toml"

parsed="$(awk '
  $0 == "[workspace.package]" { inpkg=1; next }
  inpkg && $0 ~ /^version = / {
    gsub(/^version = "/, "");
    gsub(/"$/, "");
    print;
    exit
  }
  inpkg && $0 ~ /^\[/ { exit }
' "$cargo_toml")"

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
