#!/usr/bin/env bash
set -euo pipefail

PROM_URL="${PROM_URL:-http://localhost:9090}"
W="${1:-30m}"

q() {
  local expr="$1"
  curl -sG "$PROM_URL/api/v1/query" --data-urlencode "query=$expr"
}

fmt_ms() {
  local v="${1:-0}"
  awk -v v="$v" 'BEGIN {
    if (v == "" || v == "null" || v == "NaN") { print "n/a"; exit }
    if (v >= 1000) { printf "%.2fs", v/1000.0; exit }
    printf "%.0fms", v
  }'
}

fmt_n() {
  local v="${1:-0}"
  awk -v v="$v" 'BEGIN {
    if (v == "" || v == "null" || v == "NaN") { print "n/a"; exit }
    printf "%.0f", v
  }'
}

echo "== OTEL summary (window: $W) =="
echo

echo "-- Total time split --"
llm_total=$(q "sum(increase(baml_rt_llm_call_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
llm_total_cum=$(q "sum(baml_rt_llm_call_duration_ms_sum)" | jq -r '.data.result[0].value[1] // "0"')
tool_total=$(q "sum(increase(baml_rt_tool_invocation_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
tool_total_cum=$(q "sum(baml_rt_tool_invocation_duration_ms_sum)" | jq -r '.data.result[0].value[1] // "0"')
if awk -v d="$llm_total" -v c="$llm_total_cum" 'BEGIN { exit !((d+0) == 0 && (c+0) > 0) }'; then
  echo "LLM total:  $(fmt_ms "$llm_total") (window delta; cumulative $(fmt_ms "$llm_total_cum"))"
else
  echo "LLM total:  $(fmt_ms "$llm_total")"
fi
if awk -v d="$tool_total" -v c="$tool_total_cum" 'BEGIN { exit !((d+0) == 0 && (c+0) > 0) }'; then
  echo "Tool total: $(fmt_ms "$tool_total") (window delta; cumulative $(fmt_ms "$tool_total_cum"))"
else
  echo "Tool total: $(fmt_ms "$tool_total")"
fi
echo

echo "-- LLM total time by function (window delta | cumulative) --"
while IFS=$'\t' read -r fn delta cum; do
  [[ -z "${fn:-}" ]] && continue
  printf "%-45s %10s | %10s\n" "$fn" "$(fmt_ms "$delta")" "$(fmt_ms "$cum")"
done < <(
  jq -nr --argjson d "$(q "sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W]))" | jq '.data.result')" \
         --argjson c "$(q "sum by (function) (baml_rt_llm_call_duration_ms_sum)" | jq '.data.result')" '
    def to_map(xs): reduce (xs // [])[] as $x ({}; .[($x.metric.function // "unknown")] = ($x.value[1] | tonumber));
    (to_map($d)) as $dm
    | (to_map($c)) as $cm
    | (($dm + $cm) | to_entries | sort_by(-(.value)))[]
    | .key as $k
    | "\($k)\t\($dm[$k] // 0)\t\($cm[$k] // 0)"
  '
)
echo

echo "-- LLM average latency by function --"
while IFS=$'\t' read -r fn v; do
  [[ -z "${fn:-}" ]] && continue
  printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
done < <(
  q "(sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W])) / sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W]))) and on (function) (sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
)
echo

echo "-- Tool total time by tool (window delta | cumulative) --"
while IFS=$'\t' read -r tool delta cum; do
  [[ -z "${tool:-}" ]] && continue
  printf "%-45s %10s | %10s\n" "$tool" "$(fmt_ms "$delta")" "$(fmt_ms "$cum")"
done < <(
  jq -nr --argjson d "$(q "sum by (tool) (increase(baml_rt_tool_invocation_duration_ms_sum[$W]))" | jq '.data.result')" \
         --argjson c "$(q "sum by (tool) (baml_rt_tool_invocation_duration_ms_sum)" | jq '.data.result')" '
    def to_map(xs): reduce (xs // [])[] as $x ({}; .[($x.metric.tool // "unknown")] = ($x.value[1] | tonumber));
    (to_map($d)) as $dm
    | (to_map($c)) as $cm
    | (($dm + $cm) | keys_unsorted[]) as $k
    | "\($k)\t\($dm[$k] // 0)\t\($cm[$k] // 0)"
  '
)
echo

echo "-- Tool calls by tool (window delta | cumulative) --"
while IFS=$'\t' read -r tool delta cum; do
  [[ -z "${tool:-}" ]] && continue
  printf "%-45s %10s | %10s\n" "$tool" "$(fmt_n "$delta")" "$(fmt_n "$cum")"
done < <(
  jq -nr --argjson d "$(q "sum by (tool) (increase(baml_rt_tool_invocation_total[$W]))" | jq '.data.result')" \
         --argjson c "$(q "sum by (tool) (baml_rt_tool_invocation_total)" | jq '.data.result')" '
    def to_map(xs): reduce (xs // [])[] as $x ({}; .[($x.metric.tool // "unknown")] = ($x.value[1] | tonumber));
    (to_map($d)) as $dm
    | (to_map($c)) as $cm
    | (($dm + $cm) | keys_unsorted[]) as $k
    | "\($k)\t\($dm[$k] // 0)\t\($cm[$k] // 0)"
  '
)
