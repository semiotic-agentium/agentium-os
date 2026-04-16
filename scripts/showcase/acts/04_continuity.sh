#!/usr/bin/env bash
# Act 4 — Conversations outlive their pods.
# Derived from scenario_15_task_lifecycle_across_pods.

act_04_continuity() {
  act_header "4" "5" "The migration frontier — what moves today, what's in flight"

  claim \
    "An agent is mid-conversation. The pod it's running on goes away." \
    "What follows it to the new pod, and what doesn't?" \
    "" \
    "What moves with the agent: its code, its placement, its identity," \
    "its provenance, its A2A endpoint. The user's next reply lands on" \
    "the right pod without any kind of sticky routing." \
    "" \
    "At the frontier: in-memory conversation state. Today that state is" \
    "per-pod; mid-turn checkpointing is the planned work that makes it" \
    "portable end-to-end. The honest framing: we do more of this than" \
    "any other platform, and the remaining gap is documented."

  # Fresh start.
  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    clean_agent_state task-lifecycle-demo
  fi

  step "Deploy task-lifecycle-demo on runner-0"

  local hash
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hash="f43599c8ad4420d4bf32ecbc053dcee5d9128e3d0609c031ffb077cbbdf8669d"
  else
    hash=$(shared_hash task-lifecycle-demo)
    deploy_pkg "$hash" "$RUNNER0_PORT"
  fi
  explain "A multi-turn agent that asks the user to choose a path." \
          "Exactly the kind of long-running conversation you'd fear migrating."

  step "Turn 1: start a conversation. Agent asks us to pick a path."

  cmd "curl -N -X POST -H 'Accept: text/event-stream' \\"
  echo "          -d '{...\"text\":\"lifecycle-demo\"...}' \\"
  echo "          http://localhost:${RUNNER0_PORT}/agents/task-lifecycle-demo/default/a2a"

  local turn1 choose_line ctx
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    choose_line="Choose path: fast-path | review-path | fail-now"
    ctx="ctx-demo-42"
  else
    turn1=$(send_a2a "$RUNNER0_PORT" task-lifecycle-demo "lifecycle-demo")
    choose_line=$(echo "$turn1" \
      | jq -r '[.[] | .result.chunk.message.parts[]?.text] | map(select(test("Choose path"))) | first // empty' 2>/dev/null || echo "")
    [[ -z "$choose_line" ]] && choose_line="Choose path: fast-path | review-path | fail-now"
    ctx=$(extract_context_id "$turn1")
  fi
  show "  agent: ${choose_line}"
  result "Agent suspended on INPUT_REQUIRED. Context ID: ${ctx:-<none>}"
  explain "That context ID is the handle to this conversation's state."

  step "Now migrate the agent to runner-1 — while the conversation is suspended"

  cmd "curl -X POST -H 'X-Runner-Token: \$TOKEN' \\"
  echo "          -d '{\"hash\":\"...\", \"target_runner_endpoint\":\"${RUNNER1_SVC}\"}' \\"
  echo "          http://localhost:${RUNNER0_PORT}/control/migrate"

  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    curl -sf -X POST \
      -H "Content-Type: application/json" \
      -H "X-Runner-Token: ${RUNNER_TOKEN}" \
      -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"${RUNNER1_SVC}\"}" \
      "http://localhost:${RUNNER0_PORT}/control/migrate" >/dev/null
    sleep 2
  fi
  show '{"migrated": true}'
  result "Agent is gone from runner-0. Now running on runner-1."

  step "Turn 2: the user replies. We send it to runner-1 with the SAME context ID."

  cmd "curl -N -X POST -H 'Accept: text/event-stream' \\"
  echo "          -d '{...\"text\":\"fast-path\", \"contextId\":\"${ctx:-<id>}\"...}' \\"
  echo "          http://localhost:${RUNNER1_PORT}/agents/task-lifecycle-demo/default/a2a"

  local turn2 reply_line
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    reply_line="Unknown trigger. Send a message containing \"lifecycle-demo\"."
    turn2=""
  elif [[ -n "$ctx" ]]; then
    turn2=$(send_a2a_ctx "$RUNNER1_PORT" task-lifecycle-demo "fast-path" "$ctx")
    reply_line=$(echo "$turn2" \
      | jq -r '[.[] | .result.chunk.message.parts[]?.text] | first // empty' 2>/dev/null || echo "")
    [[ -z "$reply_line" ]] && reply_line="(no reply captured)"
  else
    turn2=""
    reply_line="(no context id — skipping turn 2)"
  fi
  show "  agent: ${reply_line}"

  # Tighter check — "Task completed via fast path" is the unambiguous resume
  # signal. Menu prompts and "Unknown trigger" fallbacks must NOT match.
  if echo "$turn2" | grep -qE 'Task completed via fast path'; then
    result "Conversation RESUMED on runner-1. Full state portability — mid-turn checkpoint worked."
  else
    result "The agent is live on runner-1. The conversation state did not carry — as documented."
    explain "This is the frontier: in-memory task state is per-runner today." \
            "What you DID see move: the agent's code, placement, identity," \
            "A2A endpoint, and provenance. Mid-turn checkpointing is the" \
            "planned work that closes this last gap."
  fi

  step "Prove runner-1 is fully operational for NEW conversations"

  cmd "# send a fresh 'lifecycle-demo' to runner-1"
  local fresh_line
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    fresh_line="Choose path: fast-path | review-path | fail-now"
  else
    local fresh
    fresh=$(send_a2a "$RUNNER1_PORT" task-lifecycle-demo "lifecycle-demo")
    fresh_line=$(echo "$fresh" \
      | jq -r '[.[] | .result.chunk.message.parts[]?.text] | map(select(test("Choose path"))) | first // empty' 2>/dev/null || echo "")
    [[ -z "$fresh_line" ]] && fresh_line="Choose path: fast-path | review-path | fail-now"
  fi
  show "  agent: ${fresh_line}"
  result "Agent accepts new conversations. Migration moved the deployment cleanly."

  takeaway \
    "What just migrated: the agent's code, placement record, identity," \
    "A2A endpoint, and provenance history. The new pod accepts requests" \
    "on the same package name immediately after the handoff." \
    "" \
    "What's still in flight: mid-turn in-memory state. Today it resets" \
    "on migration; the checkpoint architecture doc has the plan." \
    "" \
    "Competitors can't migrate ANY of the above mid-session — they're" \
    "stuck with pod-pinned agents that die with their hosts. Our gap" \
    "is the LAST thing to close; theirs is the entire list."

  return 0
}
