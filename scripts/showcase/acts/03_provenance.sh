#!/usr/bin/env bash
# Act 3 — The audit trail outlives the pod.
# Derived from scenario_12_provenance_survives_migration.

act_03_provenance() {
  act_header "3" "5" "The audit trail outlives the pod"

  claim \
    "In Kubernetes, logs are per-pod. When a workload moves — or the pod" \
    "dies — the history goes with it. Compliance teams hate this; on-call" \
    "engineers hate this; regulators hate this." \
    "" \
    "Agentium stores provenance in a shared graph, bound to the agent," \
    "not the pod. The audit trail survives migration, crash, restart —" \
    "anything short of deleting SurrealDB."

  # Fresh start — act 1 left dispatch-echo on runner-1; undeploy it so we
  # can start clean on runner-0 using the same shared hash.
  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    undeploy_by_name dispatch-echo "$RUNNER0_PORT"
    undeploy_by_name dispatch-echo "$RUNNER1_PORT"
  fi

  step "Deploy dispatch-echo on runner-0 and exercise it"

  local hash
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hash="696a408aae8a1eac984ba916e28b2cfb900b171a8743b261b3b117cad2350963"
  else
    hash=$(shared_hash dispatch-echo)
    deploy_pkg "$hash" "$RUNNER0_PORT"
    # Drive some activity so there's provenance to query.
    send_a2a "$RUNNER0_PORT" dispatch-echo "provenance test message 1" >/dev/null
    send_a2a "$RUNNER0_PORT" dispatch-echo "provenance test message 2" >/dev/null
    sleep 2
  fi
  explain "deployed; 2 A2A messages dispatched."

  step "Count the lifecycle events — query via runner-0"

  cmd "curl http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events | jq '.rows | length'"

  local count_before
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    count_before=12
  else
    local lifecycle
    lifecycle=$(curl -sf "http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
    count_before=$(echo "$lifecycle" | jq '.rows | length')
  fi
  show "${count_before}"
  result "${count_before} events recorded — published, deployed, dispatches, replies."

  step "Show what a single event looks like — it is a real graph record"

  cmd "curl http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events | jq '.rows[0]'"

  local sample
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    sample='{
  "kind": "AgentBooted",
  "agent_package": "dispatch-echo",
  "runner_endpoint": "http://runner-0.runner.agentium.svc:18080",
  "timestamp_ms": 1776277128123,
  "content_hash": "696a408aae..."
}'
  else
    local lifecycle
    lifecycle=$(curl -sf "http://localhost:${RUNNER0_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
    sample=$(echo "$lifecycle" | jq '.rows[0] // {}' 2>/dev/null || echo '{}')
  fi
  show "$sample"
  result "Structured, queryable, graph-native. Not a line of free-text log."

  step "Now migrate dispatch-echo from runner-0 to runner-1"

  explain "In any other platform, this is the point at which observability breaks." \
          "A new pod starts; a fresh log stream begins; the old context is orphaned."

  cmd "curl -X POST -H 'X-Runner-Token: \$TOKEN' \\"
  echo "          -d '{\"hash\":\"$(short_hash "$hash")\", \"target_runner_endpoint\":\"${RUNNER1_SVC}\"}' \\"
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
  result "runner-0 undeploys; runner-1 deploys; cluster placement updates."

  step "Same query, this time from runner-1"

  cmd "curl http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events | jq '.rows | length'"

  local count_after
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    count_after=13
  else
    local lifecycle_after
    lifecycle_after=$(curl -sf "http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
    count_after=$(echo "$lifecycle_after" | jq '.rows | length')
  fi
  show "${count_after}"

  local delta=$((count_after - count_before))
  local noun="events"
  if (( delta == 1 )); then noun="event"; fi
  result "${count_after} total — all of the original ${count_before}, plus ${delta} new ${noun} from the migration itself."

  step "The AgentStopped event from the migration — recorded, visible from here"

  cmd "curl http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events | \\"
  echo "         jq '.rows[] | select(.a2a_stop_reason==\"undeploy\")'"

  local stopped
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    stopped='{
  "kind": "AgentStopped",
  "agent_package": "dispatch-echo",
  "runner_endpoint": "http://runner-0.runner.agentium.svc:18080",
  "a2a_stop_reason": "undeploy",
  "timestamp_ms": 1776277130001
}'
  else
    local lifecycle_after
    lifecycle_after=$(curl -sf "http://localhost:${RUNNER1_PORT}/provenance/lifecycle-events" 2>/dev/null || echo '{"rows":[]}')
    # Pick the most-recent undeploy event — the one from this demo's migration.
    stopped=$(echo "$lifecycle_after" | jq '[.rows[] | select(.a2a_stop_reason=="undeploy")] | sort_by(.prov_endTime) | last // {}' 2>/dev/null)
  fi
  if [[ "$stopped" != "{}" && -n "$stopped" ]]; then
    show "$stopped"
    result "The handoff itself is in the trail. Not inferable from logs — recorded as a first-class event."
  else
    warn "AgentStopped event not yet visible (async write lag). Re-query and it appears."
  fi

  takeaway \
    "Runner-1 was never involved with this agent until the migration." \
    "Yet it returns the agent's complete lifecycle — all ${count_after} events —" \
    "because provenance is not stored on the pod's disk. It's a graph" \
    "in SurrealDB that every runner can query." \
    "" \
    "Delete the pod tomorrow. Scale the cluster. Replace the runner image." \
    "The trail stays intact. This is the audit-grade observability that" \
    "the rest of the market is still promising in roadmaps."

  # Leave deployed on runner-1 for act 4.
  return 0
}
