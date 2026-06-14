#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Build release binaries listed in release-manifest.sh for one or more Linux targets.
#
# Usage:
#   build-release-binaries.sh [--target TRIPLE] [--all-targets]
#
# Defaults to the host triple when it appears in the manifest target list.
# Uses `cargo` for the host triple and `cross` for all other manifest targets.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=""
all_targets=false

die() {
  echo "build-release-binaries: $*" >&2
  exit 2
}

usage() {
  cat <<EOF
Usage: build-release-binaries.sh [--target TRIPLE] [--all-targets]

  --target TRIPLE   build for one Rust target triple
  --all-targets     build every target listed in the release manifest
  default           build for the host triple when listed in the manifest

Builds with the \`release-dist\` Cargo profile (thin LTO, 16 codegen units,
strip) defined in the root Cargo.toml — bounded CI memory/time, and locally
reproducible via \`cargo build --profile release-dist\`. Output lands in
target/<triple>/release-dist/.

Host deps for native Linux builds: libdbus-1-dev libcap-ng-dev pkg-config
(see INSTALL.md and \`just check-host\`).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || die "--target requires a triple"
      target="$2"
      shift 2
      ;;
    --all-targets)
      all_targets=true
      shift
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

if $all_targets && [[ -n "$target" ]]; then
  die "use either --target or --all-targets, not both"
fi

command -v python3 >/dev/null 2>&1 || die "python3 required"
command -v cargo >/dev/null 2>&1 || die "cargo required"

host="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$host" ]] || die "could not detect host triple from rustc -vV"

manifest="$(bash "${root}/scripts/release/release-manifest.sh" --json)"

mapfile -t manifest_targets < <(
  python3 -c '
import json, sys
manifest = json.load(sys.stdin)
for triple in manifest["targets"]:
    print(triple)
' <<<"$manifest"
)

if [[ ${#manifest_targets[@]} -eq 0 ]]; then
  die "release manifest lists no targets"
fi

targets=()
if $all_targets; then
  targets=("${manifest_targets[@]}")
elif [[ -n "$target" ]]; then
  targets=("$target")
else
  found=false
  for triple in "${manifest_targets[@]}"; do
    if [[ "$triple" == "$host" ]]; then
      targets=("$host")
      found=true
      break
    fi
  done
  if ! $found; then
    die "host triple ${host} is not in the release manifest; pass --target or --all-targets"
  fi
fi

for triple in "${targets[@]}"; do
  ok=false
  for listed in "${manifest_targets[@]}"; do
    if [[ "$listed" == "$triple" ]]; then
      ok=true
      break
    fi
  done
  if ! $ok; then
    die "target ${triple} is not listed in the release manifest"
  fi
done

build_for_target() {
  local triple="$1"
  local runner=(cargo build)
  local cross_build=false
  if [[ "$triple" != "$host" ]]; then
    command -v cross >/dev/null 2>&1 || die "cross required for target ${triple} (host ${host})"
    runner=(cross build)
    cross_build=true
  fi

  echo "build-release-binaries: building for ${triple} via ${runner[*]}"

  # libdbus-sys / libcap-ng probe via the pkg_config crate, which aborts when
  # host != target unless PKG_CONFIG_ALLOW_CROSS=1. Point pkg-config at the
  # target's multiarch .pc dir (e.g. /usr/lib/aarch64-linux-gnu/pkgconfig,
  # installed into the cross image by Cross.toml's pre-build hook). Cross.toml
  # forwards both vars into the container via [build.env] passthrough. Scoped to
  # the build subshell so a native host build later in --all-targets is unaffected.
  local pkg_config_path="/usr/lib/${triple/-unknown-/-}/pkgconfig"

  while IFS=$'\t' read -r package bin features; do
    local -a cmd=("${runner[@]}" --profile release-dist --target "$triple" -p "$package" --bin "$bin")
    if [[ -n "$features" ]]; then
      cmd+=(--features "$features")
    fi
    echo "build-release-binaries: ${cmd[*]}"
    (
      cd "$root"
      if $cross_build; then
        # microsandbox's build.rs (prebuilt feature) installs msb + libkrunfw under
        # $HOME/.microsandbox/. Cross containers default HOME=/ (not writable). Cargo
        # artifacts land in CARGO_TARGET_DIR=/target inside the container; that mount is
        # read-write even when the project bind-mount is not, so inject container-only
        # HOME via CROSS_CONTAINER_OPTS. Do not export HOME here: cross/rustup run
        # host-side preflight before the container starts and need the real host HOME.
        local container_home_opt="--env HOME=/target/.microsandbox-home-${triple}"
        export CROSS_CONTAINER_OPTS="${CROSS_CONTAINER_OPTS:+${CROSS_CONTAINER_OPTS} }${container_home_opt}"
        export LIBCLANG_PATH="/opt/libclang-current"
        export PKG_CONFIG_ALLOW_CROSS=1
        export PKG_CONFIG_PATH="$pkg_config_path"
      fi
      "${cmd[@]}"
    )
  done < <(
    RELEASE_BUILD_MANIFEST="$manifest" python3 <<'PY'
import json
import os
import sys

manifest = json.loads(os.environ["RELEASE_BUILD_MANIFEST"])
for entry in manifest["binaries"]:
    if not entry.get("ship", True):
        continue
    features = ",".join(entry["features"])
    print(f"{entry['package']}\t{entry['bin']}\t{features}")
PY
  )
}

for triple in "${targets[@]}"; do
  build_for_target "$triple"
done

echo "build-release-binaries: done (${#targets[@]} target(s))"
