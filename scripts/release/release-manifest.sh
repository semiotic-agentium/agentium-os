#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

# Render the canonical release manifest: workspace version + release-matrix.json.
# Usage:
#   release-manifest.sh          # human-readable table (maintainers)
#   release-manifest.sh --json   # machine-readable manifest (CI / build scripts)

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
matrix="${root}/scripts/release/release-matrix.json"
json_out=false

die() {
  echo "release-manifest: $*" >&2
  exit 2
}

usage() {
  cat <<EOF
Usage: release-manifest.sh [--json]

  --json   emit merged manifest JSON on stdout
  default  print a human-readable table
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --json)
      json_out=true
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

[[ -f "$matrix" ]] || die "missing ${matrix}"

# NOTE: the rest of scripts/release/ is bash/awk; this is the one python3 user
# (chosen over jq, which isn't guaranteed on CI runners).
command -v python3 >/dev/null 2>&1 || die "python3 required"

version="$(bash "${root}/scripts/release/workspace-version.sh")"
[[ -n "$version" ]] || die "workspace version is empty"

export RELEASE_MANIFEST_VERSION="$version"
export RELEASE_MANIFEST_MATRIX="$matrix"
export RELEASE_MANIFEST_JSON="$json_out"

python3 <<'PY'
import json
import os
import sys

version = os.environ["RELEASE_MANIFEST_VERSION"]
matrix_path = os.environ["RELEASE_MANIFEST_MATRIX"]
json_out = os.environ["RELEASE_MANIFEST_JSON"] == "true"

with open(matrix_path, encoding="utf-8") as handle:
    matrix = json.load(handle)

for key in ("targets", "binaries"):
    if key not in matrix:
        sys.exit(f"release-manifest: {matrix_path} missing {key!r}")

# Validate each binary up front so JSON mode never emits a half-formed entry
# that a downstream build script would choke on.
for i, entry in enumerate(matrix["binaries"]):
    for field in ("package", "bin", "features"):
        if field not in entry:
            sys.exit(f"release-manifest: binaries[{i}] missing {field!r}")
    if not isinstance(entry["features"], list):
        sys.exit(f"release-manifest: binaries[{i}].features must be an array")

# `ship` is passed through verbatim, NOT filtered. Consumers select on it
# themselves, e.g. `release-manifest.sh --json | jq '.binaries[]|select(.ship)'`.
manifest = {
    "version": version,
    "targets": matrix["targets"],
    "binaries": matrix["binaries"],
}

if json_out:
    json.dump(manifest, sys.stdout, indent=2)
    sys.stdout.write("\n")
    sys.exit(0)

print(f"Agentium OS release manifest (version {version})")
print()
print("Targets:")
for target in manifest["targets"]:
    print(f"  {target}")
print()
print("Binaries:")
header = ("PACKAGE", "BIN", "FEATURES", "SHIP")
rows = []
for entry in manifest["binaries"]:
    features = entry["features"]
    feature_text = ",".join(features) if features else "(default)"
    ship = entry.get("ship", True)
    rows.append(
        (
            entry["package"],
            entry["bin"],
            feature_text,
            "yes" if ship else "no",
        )
    )

widths = [len(col) for col in header]
for row in rows:
    widths = [max(w, len(cell)) for w, cell in zip(widths, row)]

def fmt_row(cells: tuple[str, ...]) -> str:
    return "  ".join(cell.ljust(width) for cell, width in zip(cells, widths))

print(fmt_row(header))
print(fmt_row(tuple("-" * width for width in widths)))
for row in rows:
    print(fmt_row(row))
PY
