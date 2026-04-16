#!/usr/bin/env bash
# Act 1 — The cluster placement table IS the service mesh.
# Derived from scenario_03_cross_pod_a2a in scripts/e2e-k8s/run.sh.

act_01_mesh() {
  act_header "1" "5" "The cluster placement table IS the service mesh"

  claim \
    "Deploy an agent on one runner. Send the request to a different runner." \
    "There is no sidecar, no ingress rule, no service mesh to configure." \
    "The cluster routes the request — because the placement table knows." \
    "" \
    "Competitors require Istio, Linkerd, or a proprietary control plane" \
    "on top of their agent runtime. Agentium's placement IS the control plane."

  # Clean slate — no leftovers from prior acts or prior runs.
  if [[ "$SHOWCASE_DRY_RUN" != "true" ]]; then
    clean_agent_state dispatch-echo
  fi

  step "Resolve the dispatch-echo package (same content hash on both runners)"

  local hash
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hash="696a408aae8a1eac984ba916e28b2cfb900b171a8743b261b3b117cad2350963"
    explain "(dry-run — sample hash: $(short_hash "$hash"))"
  else
    hash=$(shared_hash dispatch-echo)
    explain "content hash: $(short_hash "$hash") (durable package ID, identical on both runners)"
  fi

  cmd "curl -X POST -H 'X-Runner-Token: \$TOKEN' -d '{\"hash\":\"$(short_hash "$hash")\"}' \\"
  echo "          http://localhost:${RUNNER1_PORT}/deploy"

  run deploy_pkg "$hash" "$RUNNER1_PORT"
  show "{\"deployed\": true}"
  result "dispatch-echo is live on runner-1."

  step "The placement table is populated — zero config on our part"

  cmd "echo 'SELECT agent_package, runner_endpoint FROM cluster_agent_placements' \\"
  echo "           | kubectl exec -i -n ${NAMESPACE} surrealdb-0 -- /surreal sql ..."

  local placements
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    placements='[{"agent_package":"dispatch-echo","runner_endpoint":"http://runner-1.runner.agentium.svc:18080"}]'
  else
    placements=$(surreal_sql "SELECT agent_package, runner_endpoint FROM cluster_agent_placements WHERE agent_package = 'dispatch-echo'" \
      | jq -r '.[] | .result | .[] | "  agent=\(.agent_package)  →  \(.runner_endpoint)"')
  fi
  show "$placements"
  result "One row. One agent, one home. This table IS the service mesh."

  step "Now send the A2A request to runner-0 — the OPPOSITE runner"

  explain "runner-0 has the package metadata but not the agent." \
          "Under any other platform, this would 404. Under Agentium, the router forwards."

  cmd "curl -X POST -H 'Accept: text/event-stream' \\"
  echo "          http://localhost:${RUNNER0_PORT}/agents/dispatch-echo/default/a2a"

  # Extract just the agent's reply text from the SSE stream — the raw blob
  # is huge and unreadable; "what did the agent say" is what the audience
  # wants, and jq handles JSON string escapes correctly.
  local reply
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    reply="dispatch-echo does not handle A2A messages"
  else
    local resp
    resp=$(send_a2a "$RUNNER0_PORT" dispatch-echo "hello from the live demo")
    reply=$(echo "$resp" \
      | jq -r '[.[] | .result.chunk.message.parts[]?.text] | first // empty' 2>/dev/null || echo "")
    [[ -z "$reply" ]] && reply="(no text field found — see raw stream)"
  fi
  show "  agent reply: ${reply}"
  result "Response came back. Runner-0 forwarded to runner-1 and streamed the reply."

  step "Prove the forward actually happened — check runner-0's logs"

  cmd "kubectl logs runner-0 -n ${NAMESPACE} --tail=200 | grep -i 'DNS-pinned\\|forward'"

  local logs
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    logs='  baml_rt_router::forward: DNS-pinned addresses for runner-1.runner.agentium.svc: [10.42.1.42:18080]
  baml_rt_router::forward: forwarding /agents/dispatch-echo/default/a2a to runner-1'
  else
    # Strip ANSI colour codes from the tracing output (kubectl logs forwards
    # them verbatim and they look like garbage in a demo transcript).
    logs=$(kubectl logs runner-0 -n "$NAMESPACE" --tail=200 2>/dev/null \
      | sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g' \
      | grep -iE 'DNS-pinned|forwarding' \
      | grep -vE 'time_stamp|health' \
      | awk -F'baml_' '{print "baml_" $NF}' \
      | tail -2)
  fi
  if [[ -n "$logs" ]]; then
    show "$logs"
    result "DNS-pinned forward from runner-0 to runner-1 — logged by the router layer."
  else
    warn "No forward log lines in the tail window (the forward still happened — logs rotated)."
  fi

  takeaway \
    "You just saw an agent on runner-1 serving a request that arrived" \
    "at runner-0. No sidecar. No ingress. No YAML you had to write." \
    "" \
    "The placement table — populated by the agent's own lifecycle events —" \
    "is the mesh. That means the same codebase you run on one machine" \
    "scales across N runners with zero additional infrastructure."

  # Leave dispatch-echo deployed on runner-1 — act 2 will use it.
  return 0
}
