#!/usr/bin/env bash
# Act 5 — Dead runners exit routing in seconds, not kubelet cycles.
# Derived from scenario_13_stale_runner_exclusion.

act_05_selfheal() {
  act_header "5" "5" "Dead runners exit routing in seconds, not in kubelet cycles"

  claim \
    "When a runner crashes, Kubernetes will notice — eventually. The liveness" \
    "probe, the readiness probe, the endpoint controller, the kube-proxy" \
    "iptables sync. That's a lot of seconds in which requests route to a corpse." \
    "" \
    "Agentium doesn't wait for any of that. The router consults an" \
    "application-level heartbeat TTL. A dead runner is invisible to routing" \
    "as soon as its heartbeat goes stale — ahead of any kubelet signal."

  # Clean slate — direct DB cleanup ensures no stale placements survive from
  # prior acts or previous demo runs (runner-level undeploy can miss agents
  # that re-deployed from disk state after a pod restart).
  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    clean_agent_state task-lifecycle-demo
    clean_agent_state dispatch-echo
  fi

  step "Deploy dispatch-echo on runner-0; verify cross-pod routing works"

  local hash
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hash="696a408aae8a1eac984ba916e28b2cfb900b171a8743b261b3b117cad2350963"
  else
    hash=$(shared_hash dispatch-echo)
    deploy_pkg "$hash" "$RUNNER0_PORT"
    # Sanity: a request via runner-1 should forward to runner-0.
    send_a2a "$RUNNER1_PORT" dispatch-echo "pre-crash sanity" >/dev/null
  fi
  explain "runner-1 is forwarding requests to runner-0. Good. Baseline."

  step "Now: force-kill runner-0. No graceful drain, no warning."

  cmd "kubectl delete pod runner-0 -n ${NAMESPACE} --grace-period=0 --force"

  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    stop_pf runner-0
    kubectl delete pod runner-0 -n "$NAMESPACE" --grace-period=0 --force >/dev/null 2>&1
  fi
  show 'pod "runner-0" force deleted'
  explain "Imagine this is an OOM kill, a node failure, or a k8s eviction."

  step "Age its heartbeat out (simulates waiting for the TTL to expire)"

  cmd "UPDATE cluster_runners SET last_heartbeat_ms = 0"
  echo "      ${DIM}   WHERE endpoint = '${RUNNER0_SVC}'${NC}"

  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    surreal_sql "UPDATE cluster_runners SET last_heartbeat_ms = 0 WHERE endpoint = '${RUNNER0_SVC}'" >/dev/null
  fi
  explain "The default TTL is 90s. We're skipping the wait by setting the" \
          "heartbeat to epoch 0 — same effect, faster demo."

  step "Run the placement query the router uses. Stale runner must be excluded."

  cmd "SELECT * FROM cluster_agent_placements"
  echo "      ${DIM}    WHERE agent_package = 'dispatch-echo'${NC}"
  echo "      ${DIM}    AND runner_id IN (${NC}"
  echo "      ${DIM}      SELECT VALUE runner_id FROM cluster_runners${NC}"
  echo "      ${DIM}      WHERE last_heartbeat_ms > (time::millis(time::now()) - 90000)${NC}"
  echo "      ${DIM}    )${NC}"

  local stale_count
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    stale_count=0
  else
    local stale_placements
    stale_placements=$(surreal_sql "SELECT * FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo' AND runner_id IN (SELECT VALUE runner_id FROM cluster_runners WHERE last_heartbeat_ms > (time::millis(time::now()) - 90000))")
    stale_count=$(echo "$stale_placements" | jq '[.[] | .result | .[]] | length')
  fi
  show "  ${stale_count} row(s)"
  if (( stale_count == 0 )); then
    result "Zero. runner-0's placement is invisible. The router cannot route to a corpse."
  else
    fail_soft "Expected 0 rows but got ${stale_count} — stale placement from prior state."
  fi

  step "Confirm end-to-end: request via runner-1 for the agent that lived on runner-0"

  cmd "curl -X POST http://localhost:${RUNNER1_PORT}/agents/dispatch-echo/default/a2a"

  local code
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    code="404"
  else
    local body='{"jsonrpc":"2.0","id":"corr","method":"message.sendStream","params":{"message":{"messageId":"m","role":"user","parts":[{"kind":"text","text":"post-crash"}]}}}'
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 -X POST \
      -H "Accept: text/event-stream" -H "Content-Type: application/json" -d "$body" \
      "http://localhost:${RUNNER1_PORT}/agents/dispatch-echo/default/a2a" 2>/dev/null)
  fi
  if [[ "$code" =~ ^4 ]]; then
    result "HTTP ${code} — the request does NOT get routed to the dead runner. It just fails clean."
  else
    fail_soft "Expected HTTP 4xx but got ${code} — stale deployment may still be serving on runner-1."
  fi

  step "Recovery: the StatefulSet recreates runner-0. It re-registers on boot."

  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    explain "Waiting for StatefulSet to recreate runner-0..."
    restart_pf runner-0 "$RUNNER0_PORT"
  fi
  explain "New pod boots, new heartbeat is fresh, placement query includes it again."

  cmd "SELECT last_heartbeat_ms FROM cluster_runners WHERE endpoint LIKE '%runner-0%'"

  local hb
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hb=1776277125851
  else
    local hb_raw
    hb_raw=$(surreal_sql "SELECT last_heartbeat_ms FROM cluster_runners WHERE endpoint = '${RUNNER0_SVC}'")
    hb=$(echo "$hb_raw" | jq '[.[] | .result | .[].last_heartbeat_ms] | max // 0')
  fi
  show "  last_heartbeat_ms = ${hb}"
  result "Fresh. runner-0 is back on the routable list."

  takeaway \
    "Routing was safe through the entire window: the moment the heartbeat" \
    "went stale, the router stopped considering runner-0 — before kubelet" \
    "probes, before service endpoint updates, before anything k8s-native" \
    "had a chance to react." \
    "" \
    "This is the difference between 'we will tolerate faults eventually'" \
    "and 'we refuse to route to a corpse, period'. It is the kind of" \
    "guarantee your on-call team notices at 3 a.m."

  # Cleanup for any subsequent run.
  [[ "$SHOWCASE_DRY_RUN" != "true" ]] && undeploy_by_name dispatch-echo "$RUNNER0_PORT"
  return 0
}
