#!/usr/bin/env bash
# Defense-in-depth lint enforcing the visibility convention codified at
# `crates/baml-rt-provenance/src/lib.rs` and
# `crates/baml-rt-provenance/src/metamodel/mod.rs`:
#
#   Inside `crates/baml-rt-provenance/src/`, every SurrealQL string
#   targeting `prov_node` or `prov_edge` MUST be produced by
#   `metamodel::GraphQuery::into_surreal` or
#   `metamodel::EdgeProjection::into_surreal`. Hand-rolled multi-hop
#   traversals via `format!`-of-`semantic_labels::WAS_*` (or
#   `a2a_relations::*` / `GraphNodeLabel::<X>::as_str()`) are
#   prohibited.
#
# This script walks `crates/baml-rt-provenance/src/` and rejects any of:
#   - `use .*semantic_labels` / `use .*a2a_relations`
#     / `use .*context_scope` (raw vocabulary imports).
#   - `semantic_labels::WAS_<NAME>` / `context_scope::SCOPED_TO`
#     / `a2a_relations::TASK_*` / `a2a_relations::MESSAGE_*` references
#     in any non-metamodel code position (doc-comment-only matches are
#     filtered out).
#   - `GraphNodeLabel::<Variant>::as_str()` interpolation.
#
# # Exempt files
#
# The metamodel module is the only legal place where these constants
# may be used to BUILD SQL strings. The vocabulary module defines the
# constants. The files below remain exempt today because they have not
# yet been migrated to the typed surface; each migration removes its
# exemption and locks in the typed-surface invariant for that file:
#
# - `surreal_write_batch.rs`, `prov_write_semantics.rs`, `normalizer.rs`
#   — write-side helpers (pending typed `MetamodelWriter` migration).
# - `graph_export/**`, `simplify.rs`, `sequence.rs` — non-SQL graph
#   rendering and sequence diagrams.
# - `episode/**` — episode aggregation queries.
# - `surreal_store/context_reader.rs`, `surreal_store/planning_query.rs`,
#   `surreal_store/mod.rs` — `ConversationReadModel` and adjacent
#   read paths.
# - `context_metrics_queries.rs`, `citation_queries.rs` — read-side
#   helpers.
# - `events.rs`, `graph_model.rs`, `lib.rs` — doc comments and constant
#   re-exports only; not SQL-building call sites.
#
# Usage: from repo root. No args: scan the entire provenance crate. Or
# pass file paths (pre-commit hook integration).
set -euo pipefail

root=$(git rev-parse --show-toplevel 2>/dev/null || true)
if [[ -z "$root" ]]; then
  root="."
fi
cd "$root"

if ! command -v rg &>/dev/null; then
  echo >&2 "check-no-raw-graph-strings: ripgrep (rg) is required"
  exit 1
fi

scan_root="crates/baml-rt-provenance/src"

# Files / globs exempt from the lint.
EXEMPT_GLOBS=(
  "${scan_root}/metamodel/**"
  "${scan_root}/vocabulary.rs"
  "${scan_root}/lib.rs"
  "${scan_root}/events.rs"
  "${scan_root}/graph_model.rs"
  "${scan_root}/surreal_sql.rs"
  "${scan_root}/surreal_write_batch.rs"
  "${scan_root}/prov_write_semantics.rs"
  "${scan_root}/normalizer.rs"
  "${scan_root}/citation_queries.rs"
  "${scan_root}/context_metrics_queries.rs"
  "${scan_root}/conversation_history_resume.rs"
  "${scan_root}/episode/**"
  "${scan_root}/graph_export/**"
  "${scan_root}/surreal_store/context_reader.rs"
  "${scan_root}/surreal_store/planning_query.rs"
  "${scan_root}/surreal_store/mod.rs"
)

GLOB_ARGS=()
for g in "${EXEMPT_GLOBS[@]}"; do
  GLOB_ARGS+=(--glob "!${g}")
done

# Resolve target file set.
TARGET_ARGS=()
if [[ $# -gt 0 ]]; then
  for f in "$@"; do
    case "$f" in
    "${scan_root}"/*)
      skip=0
      for g in "${EXEMPT_GLOBS[@]}"; do
        case "$f" in
        ${g}) skip=1; break ;;
        esac
      done
      if [[ $skip -eq 0 ]]; then
        TARGET_ARGS+=("$f")
      fi
      ;;
    esac
  done
  if [[ ${#TARGET_ARGS[@]} -eq 0 ]]; then
    exit 0
  fi
else
  TARGET_ARGS+=("$scan_root")
fi

# Filter rg output to drop matches whose code position is a comment
# line (doc comments mentioning a constant name should not trigger the
# lint). Format of rg -n output: `path:line:content`.
filter_non_comment() {
  awk -F':' '{
    rest = ""
    for (i = 3; i <= NF; i++) {
      rest = rest (i > 3 ? ":" : "") $i
    }
    # Strip leading whitespace, skip if line starts with `//` (single-
    # or doc-comment) — non-comment matches are reported.
    n = rest
    sub(/^[ \t]+/, "", n)
    if (substr(n, 1, 2) != "//") {
      print
    }
  }'
}

bad=0

# 1. Forbidden imports of the raw vocabulary modules.
#    `-H` forces filename in output so the awk filter sees a uniform
#    `path:line:content` shape regardless of single-file vs.
#    multi-file invocation.
matches=$(rg -nH --type rust \
  "${GLOB_ARGS[@]}" \
  '^\s*use\s+.*(semantic_labels|a2a_relations|context_scope)' \
  "${TARGET_ARGS[@]}" 2>/dev/null | filter_non_comment || true)
if [[ -n "$matches" ]]; then
  echo >&2 "Forbidden raw-vocabulary import inside baml-rt-provenance/src/ (must route through metamodel):"
  echo >&2 "$matches"
  bad=1
fi

# 2. Direct interpolation of WAS_* / SCOPED_TO / TASK_* / MESSAGE_*
#    edge constants.
matches=$(rg -nH --type rust \
  "${GLOB_ARGS[@]}" \
  '(semantic_labels::(WAS_|SCOPED_TO)|a2a_relations::(TASK_|MESSAGE_)|context_scope::SCOPED_TO)' \
  "${TARGET_ARGS[@]}" 2>/dev/null | filter_non_comment || true)
if [[ -n "$matches" ]]; then
  echo >&2 "Forbidden raw graph-label reference inside baml-rt-provenance/src/ (route through metamodel::SemanticEdge / GraphQuery / EdgeProjection):"
  echo >&2 "$matches"
  bad=1
fi

# 3. `GraphNodeLabel::<X>::as_str()` interpolation for SQL building.
matches=$(rg -nH --type rust \
  "${GLOB_ARGS[@]}" \
  'GraphNodeLabel::[A-Z][A-Za-z0-9]*\.as_str\(\)' \
  "${TARGET_ARGS[@]}" 2>/dev/null | filter_non_comment || true)
if [[ -n "$matches" ]]; then
  echo >&2 "Forbidden GraphNodeLabel::*::as_str() interpolation inside baml-rt-provenance/src/ (use metamodel::labels::<X>::LABEL_STR or GraphQuery<labels::X, _>):"
  echo >&2 "$matches"
  bad=1
fi

# 4. Excised relational-shadow tables. The `a2a_task` /
#    `a2a_message` / `a2a_update` mirrors and the `TBL_A2A_*`
#    constants were removed in the A2A relational-shadow excision
#    (Phase D). Any reintroduction of a `FROM a2a_*` SQL fragment
#    or a `TBL_A2A_*` constant is a regression: tasks, messages,
#    statuses, and artifacts now live exclusively in the provenance
#    graph (`prov_node` + `prov_edge`) and must be read through
#    `TaskGraphReader`. Live updates flow through
#    `TaskUpdateBroadcaster`, never through a relational queue.
#
#    The check spans the whole repo (not just provenance/src/) because
#    the relational shadow used to leak into integration tests, the
#    runner, and the API layer.
matches=$(rg -nH --type rust \
  '(\bFROM\s+a2a_(task|message|update)\b|\bUPSERT\s+a2a_(task|message|update)\b|\bUPDATE\s+a2a_(task|message|update)\b|\bDELETE\s+FROM\s+a2a_(task|message|update)\b|\bTBL_A2A_(TASK|MESSAGE|UPDATE)\b)' \
  -- crates 2>/dev/null | filter_non_comment || true)
if [[ -n "$matches" ]]; then
  echo >&2 "Forbidden reference to excised a2a_* relational shadow tables / TBL_A2A_* constants:"
  echo >&2 "$matches"
  echo >&2 "Use baml_rt_provenance::TaskGraphReader for reads and the prov_node/prov_edge graph surface for writes. The shadow tables were removed in the A2A Relational Shadow Excision (Phase D)."
  bad=1
fi

exit "$bad"
