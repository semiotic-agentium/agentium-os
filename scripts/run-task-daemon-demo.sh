#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

CHANNEL="${TASK_DAEMON_DEMO_CHANNEL:-agentium-eng}"
COORDINATOR_PORT="${TASK_DAEMON_DEMO_COORDINATOR_PORT:-8082}"
COORDINATOR_URL="${TASK_DAEMON_DEMO_COORDINATOR_URL:-http://127.0.0.1:${COORDINATOR_PORT}}"
PROVENANCE_DB="${TASK_DAEMON_DEMO_PROVENANCE_DB:-provenance.db}"
STATE_FILE="${TASK_DAEMON_DEMO_STATE_FILE:-/tmp/task-daemon-demo-state.json}"
JSONL_OUT="${TASK_DAEMON_DEMO_JSONL:-/tmp/task-daemon-demo-batch.jsonl}"
RUN_LOG="${TASK_DAEMON_DEMO_LOG:-/tmp/task-daemon-demo.log}"
MERMAID_OUT="${TASK_DAEMON_DEMO_MERMAID_OUT:-/tmp/task-daemon-demo-sequence.mmd}"
METRICS_OUT="${TASK_DAEMON_DEMO_METRICS_OUT:-/tmp/task-daemon-demo-metrics.json}"
SUMMARY_OUT="${TASK_DAEMON_DEMO_SUMMARY_OUT:-/tmp/task-daemon-demo-scoreboard.md}"
REPORT_OUT="${TASK_DAEMON_DEMO_REPORT_OUT:-/tmp/task-daemon-demo-report.html}"
PROJECT_CONFIG="${TASK_DAEMON_DEMO_PROJECT_CONFIG:-.agentium/task-daemon-projects.json}"
EXTRACTOR="${TASK_DAEMON_DEMO_EXTRACTOR:-llm}"
MAX_CANDIDATES="${TASK_DAEMON_DEMO_MAX_CANDIDATES:-4}"
START_COORDINATOR="${TASK_DAEMON_DEMO_START_COORDINATOR:-1}"
AUTH_MODE="${TASK_DAEMON_DEMO_AUTH:-auto}"
RESET_STATE="${TASK_DAEMON_DEMO_RESET_STATE:-1}"
SPECIALIST_PROFILE="${TASK_DAEMON_DEMO_SPECIALIST_PROFILE:-clickup}"
CONTEXT_ID_OVERRIDE="${TASK_DAEMON_DEMO_CONTEXT_ID:-}"
COORDINATOR_LOG="${TASK_DAEMON_DEMO_COORDINATOR_LOG:-/tmp/coordinator-runner.log}"
SKIP_POLL="${TASK_DAEMON_DEMO_SKIP_POLL:-0}"
ALLOW_MISSING_CONTEXT="${TASK_DAEMON_DEMO_ALLOW_MISSING_CONTEXT:-0}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required but not found in PATH" >&2
  exit 1
fi
if ! command -v rg >/dev/null 2>&1; then
  echo "rg is required but not found in PATH" >&2
  exit 1
fi

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

if [ "$RESET_STATE" = "1" ]; then
  rm -f "$STATE_FILE" "$JSONL_OUT" "$RUN_LOG" "$COORDINATOR_LOG" "$METRICS_OUT" "$SUMMARY_OUT" "$REPORT_OUT"
fi

COORDINATOR_LOG_START_LINE=0
if [ -f "$COORDINATOR_LOG" ]; then
  COORDINATOR_LOG_START_LINE="$(wc -l < "$COORDINATOR_LOG" | tr -d '[:space:]')"
fi

case "$SPECIALIST_PROFILE" in
  clickup)
    DEFAULT_INCLUDE_CLICKUP=1
    DEFAULT_INCLUDE_NOTION=0
    ;;
  notion)
    DEFAULT_INCLUDE_CLICKUP=0
    DEFAULT_INCLUDE_NOTION=1
    ;;
  both)
    DEFAULT_INCLUDE_CLICKUP=1
    DEFAULT_INCLUDE_NOTION=1
    ;;
  none|coordinator_only)
    DEFAULT_INCLUDE_CLICKUP=0
    DEFAULT_INCLUDE_NOTION=0
    ;;
  *)
    echo "Invalid TASK_DAEMON_DEMO_SPECIALIST_PROFILE=$SPECIALIST_PROFILE (expected: clickup|notion|both|none)" >&2
    exit 1
    ;;
esac

INCLUDE_CLICKUP="${COORDINATOR_DEMO_INCLUDE_CLICKUP:-$DEFAULT_INCLUDE_CLICKUP}"
INCLUDE_NOTION="${COORDINATOR_DEMO_INCLUDE_NOTION:-$DEFAULT_INCLUDE_NOTION}"

if [ "$INCLUDE_CLICKUP" = "1" ] && [ -z "${CLICKUP_API_KEY:-}" ]; then
  echo "Warning: CLICKUP_API_KEY is not set but ClickUp specialist is enabled; coordinator may request clarification instead of delegating." >&2
fi

if [ "$START_COORDINATOR" = "1" ]; then
  COORDINATOR_DEMO_NO_STREAM=1 \
  COORDINATOR_DEMO_PORT="$COORDINATOR_PORT" \
  COORDINATOR_DEMO_PROVENANCE_DB="$PROVENANCE_DB" \
  COORDINATOR_DEMO_INCLUDE_CLICKUP="$INCLUDE_CLICKUP" \
  COORDINATOR_DEMO_INCLUDE_NOTION="$INCLUDE_NOTION" \
  ./scripts/run-coordinator-demo.sh
fi

if [ "$SKIP_POLL" = "1" ]; then
  echo "Skipping Slack poll/task-daemon run (TASK_DAEMON_DEMO_SKIP_POLL=1)." >&2
else
  echo "Running task-daemon once against ${CHANNEL} (auth=${AUTH_MODE}) with workflow-intake dispatch enabled..." >&2

  RUST_LOG="${RUST_LOG:-info}" \
    cargo run -p baml-task-daemon -- run \
      --channel "$CHANNEL" \
      --auth "$AUTH_MODE" \
      --once \
      --extractor "$EXTRACTOR" \
      --max-candidates "$MAX_CANDIDATES" \
      --dispatch-base-url "$COORDINATOR_URL" \
      --dispatch-agent-package workflow-intake-agent \
      --dispatch-agent-instance-id default \
      --dispatch-live \
      --emit-empty \
      --state-file "$STATE_FILE" \
      --project-config "$PROJECT_CONFIG" \
      --jsonl-out "$JSONL_OUT" \
      --no-stdout \
    2>&1 | tee "$RUN_LOG"
fi

CONTEXT_ID="$CONTEXT_ID_OVERRIDE"
if [ -z "$CONTEXT_ID" ] && [ -f "$RUN_LOG" ]; then
  CONTEXT_ID="$((
    rg -o 'context_id=[^ ]+' "$RUN_LOG" || true
  ) | tail -n 1 | cut -d= -f2 | tr -d '\"')"
fi

if [ -z "$CONTEXT_ID" ] && [ -f "$RUN_LOG" ]; then
  CONTEXT_ID="$((
    rg -o 'ctx-[0-9]+-[0-9]+' "$RUN_LOG" || true
  ) | tail -n 1 | tr -d '\"')"
fi

if [ -z "$CONTEXT_ID" ] && [ -f "$COORDINATOR_LOG" ]; then
  CONTEXT_ID="$(
    sed -n "$((COORDINATOR_LOG_START_LINE + 1)),\$p" "$COORDINATOR_LOG" \
      | rg -o 'ctx-[0-9]+-[0-9]+' \
      | tail -n 1 || true
  )"
fi

if [ -z "$CONTEXT_ID" ]; then
  echo "No context_id captured from task-daemon log output." >&2
  echo "Inspect log: $RUN_LOG" >&2
  echo "Inspect coordinator log: $COORDINATOR_LOG" >&2
  echo "Coordinator may still have processed the handoff; check runner log for details." >&2
  if [ "$ALLOW_MISSING_CONTEXT" = "1" ]; then
    echo "Continuing because TASK_DAEMON_DEMO_ALLOW_MISSING_CONTEXT=1." >&2
    exit 0
  fi
  exit 1
fi

echo "Captured context id: $CONTEXT_ID" >&2
echo "Fetching context metrics timeline..." >&2
METRICS_URL="${COORDINATOR_URL}/contexts/${CONTEXT_ID}/metrics"
TMP_METRICS="$(mktemp)"
TMP_METRICS_ERR="$(mktemp)"
if curl -fsS "$METRICS_URL" >"$TMP_METRICS" 2>"$TMP_METRICS_ERR"; then
  rm -f "$TMP_METRICS_ERR"
  mv "$TMP_METRICS" "$METRICS_OUT"
  jq . "$METRICS_OUT"
else
  if [ -s "$TMP_METRICS_ERR" ]; then
    echo "Metrics fetch error detail:" >&2
    sed 's/^/  /' "$TMP_METRICS_ERR" >&2
  fi
  rm -f "$TMP_METRICS_ERR"
  rm -f "$TMP_METRICS"
  echo "Could not fetch context metrics from ${METRICS_URL}; continuing without live metrics." >&2
  if [ -f "$METRICS_OUT" ]; then
    echo "Reusing existing metrics artifact: $METRICS_OUT" >&2
  else
    echo "No prior metrics artifact found; scoreboard/report will omit live metrics." >&2
  fi
fi

echo "Writing demo scoreboard to ${SUMMARY_OUT}..." >&2
{
  echo "# Task Daemon Demo Scoreboard"
  echo
  echo "- Context ID: \`$CONTEXT_ID\`"
  echo "- Channel: \`$CHANNEL\`"
  echo "- Extractor: \`$EXTRACTOR\`"
  echo "- Max candidates: \`$MAX_CANDIDATES\`"
  echo "- Specialist profile: \`$SPECIALIST_PROFILE\`"
  echo "- Loaded specialists (clickup/notion): \`$INCLUDE_CLICKUP/$INCLUDE_NOTION\`"
  echo "- Coordinator URL: \`$COORDINATOR_URL\`"

  if [ -f "$METRICS_OUT" ]; then
    jq -r '
      "- Turns: \(.session.turns_total // 0)",
      "- User prompts: \(.session.user_prompts_total // 0)",
      "- LLM calls: \(.session.llm_calls_total // 0)",
      "- LLM duration (ms): \(.session.llm_duration_ms_total // 0)",
      "- Tokens in/out/total: \(.session.tokens_total.in // 0)/\(.session.tokens_total.out // 0)/\(.session.tokens_total.total // 0)"
    ' "$METRICS_OUT"
  fi

  if [ -f "$JSONL_OUT" ]; then
    echo "- Messages scanned: $(jq -s 'map(.messages_scanned // 0) | add // 0' "$JSONL_OUT")"
    echo "- Derived tasks: $(jq -s 'map((.derived_tasks // []) | length) | add // 0' "$JSONL_OUT")"
    echo
    echo "## Executive Summary"
    jq -s -r 'last.interpretation.executive_summary // "n/a"' "$JSONL_OUT"
    echo
    echo "## Top Derived Tasks"
    jq -s -r '
      def display_title($task):
        if (($task.title // "") == "Blocking clarification") and (($task.description // "") | length > 0) then
          "Clarification needed: " + ($task.description // "")
        else
          ($task.title // "(untitled)")
        end;

      def shorten($text):
        if ($text | length) > 140 then
          ($text[0:137] + "...")
        else
          $text
        end;

      def dedupe_by_display:
        reduce .[] as $task (
          [];
          if any(.[]; display_title(.) == display_title($task)) then . else . + [$task] end
        );

      if (last.derived_tasks // [] | length) == 0 then
        "- none"
      else
        (last.derived_tasks // [] | dedupe_by_display | .[0:5][])
        | "- [" + ((.priority // "unknown") | ascii_upcase) + "] " + (shorten(display_title(.)))
      end
    ' "$JSONL_OUT"
  fi
} > "$SUMMARY_OUT"

./scripts/render-task-daemon-demo-report.sh \
  "$CONTEXT_ID" \
  "$CHANNEL" \
  "$EXTRACTOR" \
  "$COORDINATOR_URL" \
  "$JSONL_OUT" \
  "$METRICS_OUT" \
  "$MERMAID_OUT" \
  "$REPORT_OUT"

echo "Demo artifacts:" >&2
echo "- Context id: $CONTEXT_ID" >&2
echo "- Task-daemon batch JSONL: $JSONL_OUT" >&2
echo "- Context metrics JSON: $METRICS_OUT" >&2
echo "- Demo scoreboard: $SUMMARY_OUT" >&2
echo "- Demo report: $REPORT_OUT" >&2
echo "- Task-daemon run log: $RUN_LOG" >&2
echo "Stop coordinator backend: ./scripts/stop-coordinator-demo.sh" >&2
