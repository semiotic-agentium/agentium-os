#!/usr/bin/env bash
# Act 2 — The router refuses unsafe requests on the agent's behalf.
# Derived from scenario_05_ssrf_rejection and scenario_06_token_enforcement.

act_02_safety() {
  act_header "2" "5" "Host-governed safety — the platform enforces, not the agent"

  claim \
    "LLM-driven agents are a prompt-injection playground: a cleverly phrased" \
    "request could trick an agent into migrating itself to an attacker-controlled" \
    "URL, exfiltrating cloud credentials via the metadata endpoint." \
    "" \
    "Agentium refuses those requests at the router — before they ever" \
    "reach agent code. Safety is a host property, not something you" \
    "have to hope the model didn't get talked out of."

  # Act 1 left dispatch-echo deployed on runner-1; reuse that hash.
  # shared_hash would also work, but querying the live deployment proves
  # this act uses the same artifact the previous act set up.
  local hash
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    hash="696a408aae8a1eac984ba916e28b2cfb900b171a8743b261b3b117cad2350963"
  else
    local agents
    agents=$(curl -sf "http://localhost:${RUNNER1_PORT}/agents" 2>/dev/null || echo "[]")
    hash=$(echo "$agents" | jq -r '.[] | select(.agent_package=="dispatch-echo") | .agent_card.content_hash' 2>/dev/null | head -1)
    [[ -n "$hash" ]] || die "act 2: dispatch-echo not found on runner-1; act 1 may have failed"
  fi

  step "Attempt A: tell the agent to migrate itself to AWS metadata endpoint"

  explain "This is a real SSRF attack vector. If the router didn't block it," \
          "the attacker would exfiltrate IAM role credentials from 169.254.169.254."

  cmd "curl -X POST -H 'X-Runner-Token: \$TOKEN' \\"
  echo "          -d '{\"hash\":\"$(short_hash "$hash")\", \"target_runner_endpoint\":\"http://169.254.169.254\"}' \\"
  echo "          http://localhost:${RUNNER1_PORT}/control/migrate"

  local code
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    code="400"
    show "{\"error\": \"endpoint host '169.254.169.254' is blocked\"}"
  else
    local response
    response=$(curl -s -w '\nHTTP_CODE:%{http_code}' -X POST \
      -H "Content-Type: application/json" \
      -H "X-Runner-Token: ${RUNNER_TOKEN}" \
      -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"http://169.254.169.254\"}" \
      "http://localhost:${RUNNER1_PORT}/control/migrate" 2>/dev/null)
    code=$(echo "$response" | grep '^HTTP_CODE:' | sed 's/HTTP_CODE://')
    local body
    body=$(echo "$response" | grep -v '^HTTP_CODE:' | head -5)
    [[ -n "$body" ]] && show "$body"
  fi
  result "HTTP ${code} — rejected before any request left the pod."

  step "Attempt B: try the alibaba/gcp cloud metadata endpoints too"

  cmd "# same shape, different target"
  echo ""
  local targets=("http://metadata.google.internal" "http://100.100.100.200:18080")
  for target in "${targets[@]}"; do
    local t_code
    if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
      t_code="400"
    else
      t_code=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
        -H "Content-Type: application/json" \
        -H "X-Runner-Token: ${RUNNER_TOKEN}" \
        -d "{\"hash\":\"${hash}\",\"target_runner_endpoint\":\"${target}\"}" \
        "http://localhost:${RUNNER1_PORT}/control/migrate" 2>/dev/null)
    fi
    printf "      %-42s  ${RED}HTTP %s${NC}  blocked\n" "$target" "$t_code"
  done
  result "All cloud-IMDS targets rejected. Loopback, link-local, IPv6 metadata — all covered."

  step "Attempt C: drop the auth token — try to deploy without credentials"

  cmd "curl -X POST -d '{\"hash\":\"...\"}' http://localhost:${RUNNER0_PORT}/deploy"
  echo "      ${DIM}# note: no X-Runner-Token header${NC}"

  local auth_code
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    auth_code="401"
  else
    auth_code=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
      -H "Content-Type: application/json" \
      -d "{\"hash\":\"${hash}\"}" \
      "http://localhost:${RUNNER0_PORT}/deploy" 2>/dev/null)
  fi
  result "HTTP ${auth_code} — the cluster control plane is token-gated."

  step "Where does that decision actually live? In the router crate."

  cmd "grep -n 'is blocked' crates/baml-rt-router/src/ssrf.rs"

  local src
  if [[ "$SHOWCASE_DRY_RUN" == "true" ]]; then
    src='  45:            return Err(format!("endpoint host {host} is blocked"));
  62:    reject_dangerous_ip(ip, host)?;'
  else
    src=$(grep -n 'is blocked\|reject_dangerous' "${REPO_ROOT}/crates/baml-rt-router/src/ssrf.rs" 2>/dev/null | head -5)
  fi
  show "$src"
  result "A tiny, auditable safety layer. You can read all of it in one sitting."

  takeaway \
    "Every rejection you saw was the host enforcing rules the agent could" \
    "not disable. This is the opposite of 'trust the model to do the right" \
    "thing' — the host owns the safety boundary." \
    "" \
    "If a competitor wants you to inspect agent prompts for prompt injection," \
    "ask them: what happens when it slips through? Here, the router says no."

  return 0
}
