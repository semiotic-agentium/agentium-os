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
PROJECT_CONFIG="${TASK_DAEMON_DEMO_PROJECT_CONFIG:-.agentium/task-daemon-projects.json}"
EXTRACTOR="${TASK_DAEMON_DEMO_EXTRACTOR:-llm}"
START_COORDINATOR="${TASK_DAEMON_DEMO_START_COORDINATOR:-1}"
AUTH_MODE="${TASK_DAEMON_DEMO_AUTH:-auto}"
RESET_STATE="${TASK_DAEMON_DEMO_RESET_STATE:-1}"

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
  rm -f "$STATE_FILE" "$JSONL_OUT" "$RUN_LOG" "$MERMAID_OUT" "$METRICS_OUT"
fi

if [ "$START_COORDINATOR" = "1" ]; then
  COORDINATOR_DEMO_NO_STREAM=1 \
  COORDINATOR_DEMO_PORT="$COORDINATOR_PORT" \
  COORDINATOR_DEMO_PROVENANCE_DB="$PROVENANCE_DB" \
  ./scripts/run-coordinator-demo.sh
fi

echo "Running task-daemon once against ${CHANNEL} (auth=${AUTH_MODE}) with coordinator handoff enabled..." >&2

RUST_LOG="${RUST_LOG:-info}" \
  cargo run -p baml-task-daemon -- run \
    --channel "$CHANNEL" \
    --auth "$AUTH_MODE" \
    --once \
    --extractor "$EXTRACTOR" \
    --coordinator-url "$COORDINATOR_URL" \
    --a2a-live \
    --emit-empty \
    --state-file "$STATE_FILE" \
    --project-config "$PROJECT_CONFIG" \
    --jsonl-out "$JSONL_OUT" \
    --no-stdout \
  2>&1 | tee "$RUN_LOG"

CONTEXT_ID="$((
  rg -o 'context_id=[^ ]+' "$RUN_LOG" || true
) | tail -n 1 | cut -d= -f2 | tr -d '\"')"

if [ -z "$CONTEXT_ID" ]; then
  echo "No context_id captured from task-daemon log output." >&2
  echo "Inspect log: $RUN_LOG" >&2
  echo "Coordinator may still have processed the handoff; check runner log for details." >&2
  exit 0
fi

echo "Captured context id: $CONTEXT_ID" >&2
echo "Fetching context metrics timeline..." >&2
curl -fsS "${COORDINATOR_URL}/contexts/${CONTEXT_ID}/metrics" | tee "$METRICS_OUT" | jq .

echo "Exporting Mermaid sequence diagram to ${MERMAID_OUT}..." >&2
cargo run -p baml-rt-provenance --features cli --bin graph_exporter -- \
  --db "$PROVENANCE_DB" \
  --context-id "$CONTEXT_ID" \
  --simplify \
  --format mermaid \
  --output "$MERMAID_OUT"

echo "Demo artifacts:" >&2
echo "- Context id: $CONTEXT_ID" >&2
echo "- Task-daemon batch JSONL: $JSONL_OUT" >&2
echo "- Context metrics JSON: $METRICS_OUT" >&2
echo "- Mermaid sequence: $MERMAID_OUT" >&2
echo "- Task-daemon run log: $RUN_LOG" >&2
echo "Stop coordinator backend: ./scripts/stop-coordinator-demo.sh" >&2
