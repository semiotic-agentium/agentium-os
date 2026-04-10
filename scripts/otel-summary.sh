#!/usr/bin/env bash
set -euo pipefail

PROM_URL="${PROM_URL:-http://localhost:9090}"
W="${1:-30m}"

q() {
  local expr="$1"
  curl -sG "$PROM_URL/api/v1/query" --data-urlencode "query=$expr"
}

print_or_none() {
  local printed="$1"
  if [[ "$printed" -eq 0 ]]; then
    echo "(no data)"
  fi
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

fmt_ratio() {
  local v="${1:-0}"
  awk -v v="$v" 'BEGIN {
    if (v == "" || v == "null" || v == "NaN") { print "n/a"; exit }
    printf "%.2fx", v
  }'
}

echo "== OTEL summary (window: $W) =="
echo

echo "-- Total time split --"
llm_total=$(q "sum(increase(baml_rt_llm_call_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
tool_total=$(q "sum(increase(baml_rt_tool_session_plan_duration_ms_sum[$W])) + sum(increase(baml_rt_tool_invocation_duration_ms_sum[$W]))" | jq -r '.data.result[0].value[1] // "0"')
echo "LLM total:  $(fmt_ms "$llm_total")"
echo "Tool total: $(fmt_ms "$tool_total")"
echo

echo "-- LLM total time by function (desc) --"
printed=0
while IFS=$'\t' read -r fn v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
done < <(
  q "sort_desc(sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W])))" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- LLM average latency by function --"
printed=0
while IFS=$'\t' read -r fn v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
done < <(
  q "(sum by (function) (increase(baml_rt_llm_call_duration_ms_sum[$W])) / sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W]))) and on (function) (sum by (function) (increase(baml_rt_llm_call_duration_ms_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- LLM requests by function/result --"
printed=0
while IFS=$'\t' read -r fn result v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-32s %-10s %10s\n" "$fn" "$result" "$(fmt_n "$v")"
done < <(
  q "sum by (function, result) (increase(baml_rt_llm_call_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.metric.result // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- LLM token usage by function --"
printed=0
while IFS=$'\t' read -r fn tin tout; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-45s in=%-10s out=%-10s\n" "$fn" "$(fmt_n "$tin")" "$(fmt_n "$tout")"
done < <(
  join -t $'\t' -a1 -a2 -e 0 -o '0,1.2,2.2' \
    <(q "sum by (function) (increase(baml_rt_llm_tokens_in_total[$W]))" | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"' | sort) \
    <(q "sum by (function) (increase(baml_rt_llm_tokens_out_total[$W]))" | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"' | sort)
)
print_or_none "$printed"
echo

echo "-- Tool total time by tool (desc) --"
printed=0
while IFS=$'\t' read -r tool v; do
  [[ -z "${tool:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$tool" "$(fmt_ms "$v")"
done < <(
  q "sort_desc(sum by (tool) (increase(baml_rt_tool_session_plan_duration_ms_sum[$W])) + sum by (tool) (increase(baml_rt_tool_invocation_duration_ms_sum[$W])))" \
    | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Tool calls by tool --"
printed=0
while IFS=$'\t' read -r tool v; do
  [[ -z "${tool:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$tool" "$(fmt_n "$v")"
done < <(
  q "sum by (tool) (increase(baml_rt_tool_session_plan_op_total{op=\"open\"}[$W])) + sum by (tool) (increase(baml_rt_tool_invocation_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Step executor loop total time by function (host loop wall-clock) --"
printed=0
while IFS=$'\t' read -r fn v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$fn" "$(fmt_ms "$v")"
done < <(
  q "sort_desc(sum by (function) (increase(baml_rt_step_executor_loop_duration_ms_sum[$W])))" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Step executor hop counts by function/phase (LLM hop cardinality) --"
printed=0
while IFS=$'\t' read -r fn phase v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-32s %-12s %10s\n" "$fn" "$phase" "$(fmt_n "$v")"
done < <(
  q "sum by (function, phase) (increase(baml_rt_step_executor_hop_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.metric.phase // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Step executor avg hop latency by function/phase (LLM invoke duration) --"
printed=0
while IFS=$'\t' read -r fn phase v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-32s %-12s %10s\n" "$fn" "$phase" "$(fmt_ms "$v")"
done < <(
  q "(sum by (function, phase) (increase(baml_rt_step_executor_hop_latency_ms_sum[$W])) / sum by (function, phase) (increase(baml_rt_step_executor_hop_latency_ms_count[$W]))) and on (function, phase) (sum by (function, phase) (increase(baml_rt_step_executor_hop_latency_ms_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.metric.phase // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Step executor status counts by function/status (host FSM outputs) --"
printed=0
while IFS=$'\t' read -r fn status v; do
  [[ -z "${fn:-}" ]] && continue
  printed=1
  printf "%-32s %-12s %10s\n" "$fn" "$status" "$(fmt_n "$v")"
done < <(
  q "sum by (function, status) (increase(baml_rt_step_executor_status_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.function // "unknown")\t\(.metric.status // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Tool session plan total time by tool (host session-plan wall-clock) --"
printed=0
while IFS=$'\t' read -r tool v; do
  [[ -z "${tool:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$tool" "$(fmt_ms "$v")"
done < <(
  q "sort_desc(sum by (tool) (increase(baml_rt_tool_session_plan_duration_ms_sum[$W])))" \
    | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- Tool session plan op counts by tool/op (host FSM operation counts) --"
printed=0
while IFS=$'\t' read -r tool op v; do
  [[ -z "${tool:-}" ]] && continue
  printed=1
  printf "%-32s %-12s %10s\n" "$tool" "$op" "$(fmt_n "$v")"
done < <(
  q "sum by (tool, op) (increase(baml_rt_tool_session_plan_op_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.tool // "unknown")\t\(.metric.op // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- ONNX inferences by operation --"
printed=0
while IFS=$'\t' read -r op v; do
  [[ -z "${op:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$op" "$(fmt_n "$v")"
done < <(
  q "sum by (operation) (increase(baml_rt_onnx_inference_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.operation // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- ONNX avg wait by operation --"
printed=0
while IFS=$'\t' read -r op v; do
  [[ -z "${op:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$op" "$(fmt_ms "$v")"
done < <(
  q "(sum by (operation) (increase(baml_rt_onnx_wait_ms_sum[$W])) / sum by (operation) (increase(baml_rt_onnx_wait_ms_count[$W]))) and on (operation) (sum by (operation) (increase(baml_rt_onnx_wait_ms_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.operation // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- ONNX avg run by operation --"
printed=0
while IFS=$'\t' read -r op v; do
  [[ -z "${op:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$op" "$(fmt_ms "$v")"
done < <(
  q "(sum by (operation) (increase(baml_rt_onnx_run_ms_sum[$W])) / sum by (operation) (increase(baml_rt_onnx_run_ms_count[$W]))) and on (operation) (sum by (operation) (increase(baml_rt_onnx_run_ms_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.operation // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- ONNX avg wait/run ratio by operation --"
printed=0
while IFS=$'\t' read -r op v; do
  [[ -z "${op:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$op" "$(fmt_ratio "$v")"
done < <(
  q "(sum by (operation) (increase(baml_rt_onnx_wait_to_run_ratio_sum[$W])) / sum by (operation) (increase(baml_rt_onnx_wait_to_run_ratio_count[$W]))) and on (operation) (sum by (operation) (increase(baml_rt_onnx_wait_to_run_ratio_count[$W])) > 0)" \
    | jq -r '.data.result[]? | "\(.metric.operation // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
echo

echo "-- ONNX wait-dominant count by operation --"
printed=0
while IFS=$'\t' read -r op v; do
  [[ -z "${op:-}" ]] && continue
  printed=1
  printf "%-45s %10s\n" "$op" "$(fmt_n "$v")"
done < <(
  q "sum by (operation) (increase(baml_rt_onnx_wait_dominant_total[$W]))" \
    | jq -r '.data.result[]? | "\(.metric.operation // "unknown")\t\(.value[1])"'
)
print_or_none "$printed"
