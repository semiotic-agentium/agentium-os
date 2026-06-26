#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Curl-driven compaction verification against a local baml-agent-runner.
#
# Prereqs:
#   - Runner at RUNNER_URL (default http://127.0.0.1:18080) with provenance.db
#   - LLM config tuned for test (see tune_compaction_config below)
#   - conversational-context-auto or coordinator-agent deployed (post-settlement compaction)
#
# Success signals:
#   - promptContextBytesSessionCurrent plateaus while item count grows
#   - runner log contains: SummarizeConversationPrefix ... call_function: ok
set -euo pipefail

BASE="${RUNNER_URL:-http://127.0.0.1:18080}"
AGENT="${ENTRY_AGENT:-conversational-context-auto}"
CTX="${CONTEXT_ID:-ctx-compaction-curl-$(date +%s)}"
N="${MESSAGE_COUNT:-5}"
PAD_REPEAT="${PAD_REPEAT:-1500}"  # ~6k chars per turn; enough wire bytes with tuned budget
LOG="${RUNNER_LOG:-/tmp/agentium-runner.log}"

tune_compaction_config() {
  local version
  version=$(curl -sf "$BASE/config/llm" | jq -r '.version')
  curl -sf "$BASE/config/llm" | jq \
    '.config.compaction.defaults.item_threshold = 4
     | .config.compaction.defaults.recent_tail_retention = 2
     | .config.compaction.defaults.defer_while_awaiting_input = false
     | .config.compaction.client_overrides.OpenRouter = {
         context_window_tokens: 8192,
         trigger_ratio: 0.35,
         emergency_ratio: 0.55,
         output_reserve_tokens: 512
       }
     | .config' \
    | curl -sS -X PUT "$BASE/config/llm" \
        -H 'content-type: application/json' \
        -H "If-Match: $version" \
        -d @- >/dev/null
  echo "LLM compaction config tuned (version was $version; redeploy agent after PUT)." >&2
}

PAD=$(python3 -c "print('FILL ' * ${PAD_REPEAT})")

send_msg() {
  local idx="$1"
  local text="$2"
  local now corr msg_id
  now=$(($(date +%s) * 1000))
  corr="corr-${now}-${idx}"
  msg_id="msg-${idx}-${now}"
  echo ">>> [$idx/$N] ctx=$CTX items_before=$(curl -sf "${BASE}/contexts/${CTX}/conversation-history" 2>/dev/null | jq '.items | length' || echo '?')" >&2
  curl -sS -X POST "${BASE}/agents/${AGENT}/default/a2a" \
    -H 'content-type: application/json' \
    -d "$(jq -n \
      --arg ctx "$CTX" \
      --arg id "$msg_id" \
      --arg corr "$corr" \
      --arg text "$text" \
      '{jsonrpc:"2.0",id:$corr,method:"message.sendStream",params:{message:{messageId:$id,contextId:$ctx,role:"user",parts:[{text:$text}]}}}')" \
    >/dev/null
  curl -sf "${BASE}/contexts/${CTX}/conversation-history" | jq --argjson idx "$idx" '{
    turn: $idx,
    items: (.items | length),
    prompt_bytes: .promptContextBytesSessionCurrent
  }'
}

if [[ "${TUNE_CONFIG:-1}" == "1" ]]; then
  tune_compaction_config
  echo "Redeploy ${AGENT} after config PUT if not already done." >&2
fi

echo "Compaction curl verify: agent=$AGENT ctx=$CTX base=$BASE" >&2

for i in $(seq 1 "$N"); do
  send_msg "$i" "Turn ${i}: remember codeword ALPHA-${i}. ${PAD}"
done

echo "--- summary ---" >&2
curl -sf "${BASE}/contexts/${CTX}/conversation-history" | jq '{
  item_count: (.items | length),
  prompt_bytes: .promptContextBytesSessionCurrent,
  note: "Operator timeline may show all messages; prompt_bytes plateau + SummarizeConversationPrefix in logs confirms compaction"
}'

echo "--- summarizer invocations (last 5) ---" >&2
if [[ -f "$LOG" ]]; then
  rg 'SummarizeConversationPrefix.*call_function: ok' "$LOG" | tail -5 || echo "(none in $LOG)"
else
  echo "(log not found: $LOG)"
fi

echo "--- context id ---" >&2
echo "$CTX"
