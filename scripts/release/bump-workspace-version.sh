#!/usr/bin/env bash
# Bump [workspace.package].version and Chart.yaml version/appVersion.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KIND="${1:-}"

die() {
  echo "bump-workspace-version: $*" >&2
  exit 1
}

[[ "$KIND" =~ ^(patch|minor|major)$ ]] || die "usage: bump-workspace-version.sh patch|minor|major"

CARGO="${ROOT}/Cargo.toml"
CHART="${ROOT}/deploy/helm/agentium-os/Chart.yaml"

current="$(bash "${ROOT}/scripts/release/workspace-version.sh")"
IFS=. read -r major minor patch <<<"$current"

case "$KIND" in
  patch) patch=$((patch + 1)) ;;
  minor) minor=$((minor + 1)); patch=0 ;;
  major) major=$((major + 1)); minor=0; patch=0 ;;
esac
next="${major}.${minor}.${patch}"

perl -0pi -e 's/(^\[workspace\.package\]\n(?:.*\n)*?version = ")[^"]+(")/${1}'"${next}"'${2}/m' "${CARGO}"
perl -pi -e 's/^version: .*/version: '"${next}"'/' "${CHART}"
perl -pi -e 's/^appVersion: .*/appVersion: "'"${next}"'"/' "${CHART}"

echo "bump-workspace-version: ${current} → ${next}"
