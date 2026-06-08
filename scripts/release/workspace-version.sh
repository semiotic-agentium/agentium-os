#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cargo_toml="${root}/Cargo.toml"

awk '
  $0 == "[workspace.package]" { inpkg=1; next }
  inpkg && $0 ~ /^version = / {
    gsub(/^version = "/, "");
    gsub(/"$/, "");
    print;
    exit
  }
  inpkg && $0 ~ /^\[/ { exit }
' "$cargo_toml"
