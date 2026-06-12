#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Pack a per-target release tarball from built release binaries.
#
# Usage:
#   pack-target-tarball.sh --target TRIPLE [--output-dir DIR]
#
# Expects binaries at target/<TRIPLE>/release-dist/<bin> (see build-release-binaries.sh).
# Writes dist/release/agentium-os-v<version>-<TRIPLE>.tar.gz by default.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=""
output_dir="${root}/dist/release"

die() {
  echo "pack-target-tarball: $*" >&2
  exit 2
}

usage() {
  cat <<EOF
Usage: pack-target-tarball.sh --target TRIPLE [--output-dir DIR]

  --target TRIPLE     Rust target triple (required)
  --output-dir DIR    directory for the .tar.gz (default: dist/release)

Archive contents:
  - shipped release binaries
  - SHA256SUMS (hashes of the binaries)
  - INSTALL.md (copied from repo root)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || die "--target requires a triple"
      target="$2"
      shift 2
      ;;
    --output-dir)
      [[ $# -ge 2 ]] || die "--output-dir requires a path"
      output_dir="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1 (try --help)"
      ;;
  esac
done

[[ -n "$target" ]] || die "--target is required (try --help)"
command -v python3 >/dev/null 2>&1 || die "python3 required"

install_md="${root}/INSTALL.md"
[[ -f "$install_md" ]] || die "missing ${install_md}"

version="$(bash "${root}/scripts/release/workspace-version.sh")"
[[ -n "$version" ]] || die "workspace version is empty"

manifest="$(bash "${root}/scripts/release/release-manifest.sh" --json)"

mapfile -t bins < <(
  python3 -c '
import json, sys
manifest = json.load(sys.stdin)
for entry in manifest["binaries"]:
    if entry.get("ship", True):
        print(entry["bin"])
' <<<"$manifest"
)

if [[ ${#bins[@]} -eq 0 ]]; then
  die "release manifest lists no shipped binaries"
fi

# Built by the `release-dist` Cargo profile (see build-release-binaries.sh).
bin_root="${root}/target/${target}/release-dist"
missing=()
for bin in "${bins[@]}"; do
  if [[ ! -f "${bin_root}/${bin}" ]]; then
    missing+=("${bin_root}/${bin}")
  fi
done
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "pack-target-tarball: missing built binaries (run build-release-binaries.sh first):" >&2
  for path in "${missing[@]}"; do
    echo "  - ${path}" >&2
  done
  exit 1
fi

staging="$(mktemp -d "${TMPDIR:-/tmp}/agentium-release-pack.XXXXXX")"
cleanup() {
  rm -rf "$staging"
}
trap cleanup EXIT

for bin in "${bins[@]}"; do
  install -m 755 "${bin_root}/${bin}" "${staging}/${bin}"
done
install -m 644 "$install_md" "${staging}/INSTALL.md"

(
  cd "$staging"
  sha256sum "${bins[@]}" >SHA256SUMS
)
chmod 644 "${staging}/SHA256SUMS"

mkdir -p "$output_dir"
archive_name="agentium-os-v${version}-${target}.tar.gz"
archive_path="${output_dir}/${archive_name}"

# Reproducible archive: stable entry order, no uid/gid/mtime leakage, and
# `gzip -n` so the gzip header carries no timestamp/filename. Same inputs ->
# byte-identical .tar.gz (enables checksum comparison / future signing).
# Requires GNU tar (--sort/--owner/--numeric-owner); CI and the Linux targets
# both have it. Override the timestamp via SOURCE_DATE_EPOCH if desired.
tar --version 2>/dev/null | grep -q 'GNU tar' || die "GNU tar required for reproducible archives"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${SOURCE_DATE_EPOCH:-0}" \
  -C "$staging" -cf - . | gzip -9 -n >"$archive_path"

echo "pack-target-tarball: wrote ${archive_path}"
echo "pack-target-tarball: contents:"
tar -tzf "$archive_path" | sed 's/^/  /'
