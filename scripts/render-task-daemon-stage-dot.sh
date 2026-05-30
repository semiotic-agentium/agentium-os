#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

if [ "$#" -lt 2 ]; then
  cat >&2 <<'USAGE'
Usage:
  ./scripts/render-task-daemon-stage-dot.sh <input.dot> <output.dot>

Optional env:
  TASK_DAEMON_STAGE_DOT_EXCLUDE_PREFIXES
    Space-delimited node-id prefixes to remove from stage view.
    Default: "task_state: message_processing: tool_args:"
USAGE
  exit 1
fi

INPUT_DOT="$1"
OUTPUT_DOT="$2"

if [ ! -f "$INPUT_DOT" ]; then
  echo "Input DOT file not found: $INPUT_DOT" >&2
  exit 1
fi

EXCLUDE_PREFIXES="${TASK_DAEMON_STAGE_DOT_EXCLUDE_PREFIXES:-task_state: message_processing: tool_args:}"

awk -v exclude_prefixes="$EXCLUDE_PREFIXES" '
BEGIN {
  split(exclude_prefixes, prefixes, " ");
}
function should_exclude_node(id,   i, pfx) {
  for (i in prefixes) {
    pfx = prefixes[i];
    if (pfx != "" && index(id, pfx) == 1) {
      return 1;
    }
  }
  return 0;
}
{
  line_count++;
  lines[line_count] = $0;

  if ($0 ~ /^[[:space:]]*"/ && $0 ~ /\[/ && $0 !~ /->/) {
    split($0, parts, "\"");
    node_id = parts[2];
    if (node_id != "" && should_exclude_node(node_id)) {
      excluded[node_id] = 1;
    }
  }
}
END {
  for (i = 1; i <= line_count; i++) {
    line = lines[i];

    if (line ~ /^[[:space:]]*rankdir=TB;/) {
      print line;
      print "    splines=polyline;";
      print "    concentrate=true;";
      print "    nodesep=0.45;";
      print "    ranksep=0.65;";
      continue;
    }

    if (line ~ /^[[:space:]]*"/ && line ~ /\[/ && line !~ /->/) {
      split(line, parts, "\"");
      node_id = parts[2];
      if (node_id in excluded) {
        continue;
      }
      print line;
      continue;
    }

    if (line ~ /->/) {
      split(line, parts, "\"");
      from_id = parts[2];
      to_id = parts[4];
      if ((from_id in excluded) || (to_id in excluded)) {
        continue;
      }
      # Remove edge labels for stage readability while preserving any other attributes.
      gsub(/label="[^"]*",?[[:space:]]*/, "", line);
      gsub(/\[[[:space:]]*,[[:space:]]*/, "[", line);
      gsub(/,[[:space:]]*,/, ",", line);
      gsub(/,[[:space:]]*\]/, "]", line);
      gsub(/\[[[:space:]]*\]/, "", line);
      print line;
      continue;
    }

    print line;
  }
}
' "$INPUT_DOT" >"$OUTPUT_DOT"

echo "Wrote stage DOT: $OUTPUT_DOT" >&2
