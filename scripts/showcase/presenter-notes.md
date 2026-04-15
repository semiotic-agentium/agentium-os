# Agentium OS Showcase — Presenter Notes

Live walkthrough companion for `scripts/showcase/demo.sh`. Use these notes to
prepare: one section per act with the opening line, what the audience should be
watching for, likely pushback from competing teams, and how to answer.

## Before you start

**Bring the cluster up first (once, ~6 minutes):**

```bash
./scripts/e2e-k8s/run.sh --keep-cluster --no-build   # warm caches → faster
```

Leave it running. Then, when you're ready to present:

```bash
./scripts/showcase/demo.sh
```

For rehearsal without executing anything:

```bash
./scripts/showcase/demo.sh --dry-run
```

For a recording (no pauses):

```bash
./scripts/showcase/demo.sh --auto | tee demo-transcript.txt
```

Individual acts:

```bash
./scripts/showcase/demo.sh --act 3
```

**The one line to close skepticism:**
> "Everything you just watched is a CI-gated test. Run `./scripts/e2e-k8s/run.sh`
> on your laptop and you get the same results in six minutes."

---

## Act 1 — The placement table IS the service mesh

**Opening line:** *"Before I run anything, tell me: how many minutes of YAML
did you expect me to write to get two agents on two pods to talk to each
other?"*

**What the audience should watch for:**
- The `cluster_agent_placements` query returns a row we did not write. The
  agent's own lifecycle event populated it.
- We send the A2A request to runner-0. The agent is on runner-1. The response
  comes back anyway.
- The runner-0 log shows a `baml_rt_router::forward: DNS-pinned addresses`
  line — proof the forward is a real network hop with safety properties, not
  an in-process shortcut.

**Expected pushback:**

| They say | You say |
|---|---|
| "That's just a reverse proxy." | It is a proxy with three non-optional properties: DNS is resolved and pinned per request (no TOCTOU), the target address is SSRF-screened, and the placement table is the only source of truth. Try any of those in a raw reverse proxy. |
| "Istio does this." | Istio gives you L7 routing and needs a service-mesh control plane, sidecar injection, and mTLS bootstrap. We give you cross-pod A2A with no sidecar, no injection, no extra deploy. The placement table IS our control plane. |
| "How does it pick the runner?" | Last-write-wins on the deploy event. For multi-runner redundancy (same agent, multiple runners), scenario 14 in the test suite shows convergence; I can run that too. |

---

## Act 2 — Host-governed safety

**Opening line:** *"LLM-driven agents are a prompt-injection playground. A
cleverly-phrased prompt could talk an agent into migrating itself to an
attacker-controlled URL. Watch what happens when I try."*

**What the audience should watch for:**
- Each blocked target returns HTTP 4xx with a specific error ("endpoint host
  X is blocked"). No timeout, no crash — a rejection that an operator could
  log and alert on.
- The grep at the end shows the decision lives in ~20 lines of Rust in
  `crates/baml-rt-router/src/ssrf.rs`. Small enough to audit.
- Token enforcement covers both `/deploy` and `/control/migrate` — not a
  per-route afterthought but a layer applied to the whole control plane.

**Expected pushback:**

| They say | You say |
|---|---|
| "Bedrock has guardrails." | Bedrock's guardrails are prompt-shaped: they tell the model what to refuse. Ours is host-shaped: we don't care what the model says — we refuse at the router. Different trust model, different threat surface. |
| "You could do this with an egress policy." | NetworkPolicy blocks IP ranges. It doesn't know that `metadata.google.internal` resolves to a dangerous range, or that a Unicode homoglyph of `localhost` is the same target. Our SSRF layer knows both because it understands URLs, not just IPs. |
| "I'd want to see this run in a real pentest." | The test suite includes IPv6, loopback, link-local, AWS/GCP/Alibaba IMDS. I'll hand you scenario 5 and you can add targets you want tested. |

---

## Act 3 — The audit trail outlives the pod

**Opening line:** *"In Kubernetes, logs are per-pod. When a workload moves,
the history goes with it. Compliance teams hate this. Let me show you what
Agentium does instead."*

**What the audience should watch for:**
- The "count: 12" on runner-0 followed by "count: 13" on runner-1 — runner-1
  returns events it never directly generated. Because it queries the same
  SurrealDB graph that runner-0 writes to.
- The sample event record has structure (kind, agent_package, runner_endpoint,
  timestamp). Graph-native, not free-text.
- The AgentStopped event with `a2a_stop_reason: "undeploy"` — the migration
  itself is a first-class event in the trail. You can ask: who ordered this
  agent off runner-0, when, and why.

**Expected pushback:**

| They say | You say |
|---|---|
| "Couldn't you just ship logs to a central store?" | Sure, but that's free-text correlated on agent IDs, and you're one crash away from losing unflushed writes. This is a graph with edges between agent, runner, and event. You can query it like a database, not grep it like a log file. |
| "What if SurrealDB goes down?" | It's the shared dependency, yes — and it runs as a StatefulSet with its own PVC, just like Postgres would. The alternative — per-pod logs — has N failure modes instead of one. We've made the failure mode you can actually operate against. |
| "What's the write path look like under load?" | The provenance crate writes events async, so agent latency is not coupled to graph write latency. Happy to walk through `crates/baml-rt-provenance/` after. |

---

## Act 4 — The migration frontier

**Opening line:** *"I want to be specific about what migrates and what
doesn't today, because 'how much survives a pod move?' is exactly the
question a skeptical engineer should ask."*

**Current behavior to set expectations:**
- **Moves:** agent code, placement record, identity, A2A endpoint, provenance.
- **In flight:** in-memory conversation state. Checkpoint architecture doc
  describes the plan.

**What the audience should watch for:**
- The context ID from turn 1 — the handle.
- Migration happens. Turn 2 goes to runner-1 with the SAME context ID.
- Two possible outcomes depending on the run:
  1. **Resumed ("Task completed via fast path")** — checkpoint worked.
  2. **Fresh ("Unknown trigger")** — mid-turn state was lost, agent
     responded from a fresh session. This is the documented current state.
- Either way, the fresh-conversation follow-up proves runner-1 is
  operationally serving the agent — the migration itself was clean.

**The honest framing — do not oversell:**
*"We can migrate the agent. We can migrate its provenance. We can migrate
its placement. What we're working on is the in-memory turn state — the
part a competitor can't migrate at all. We're one gap away from full
conversation portability; they're five gaps away from the first."*

**Expected pushback:**

| They say | You say |
|---|---|
| "So it doesn't actually work." | Depends what "it" is. Agent deployment migration works — I showed it. Provenance continuity works — I showed it. The part still in flight is mid-turn state portability, which is a well-scoped engineering problem, not a design problem. Nobody else even has the first piece. |
| "Why not just pin the agent to a pod?" | Because then you can't do any of what I showed in acts 3 and 5 — crash recovery, deterministic audit trail, self-healing routing. Pinning gets you one property at the cost of all the others. |
| "Temporal does workflow durability." | Temporal durably checkpoints deterministic workflows. LLM-driven agents aren't deterministic — the model output varies. Temporal would need wrappers to model each LLM call as an activity; we model it natively. |
| "Why would I need mid-turn migration?" | OOM kills, node drains, image rollouts, spot-instance reclaims. You don't plan for migration — it happens. The question is whether the user notices. |
| "Can you lose a message?" | In the crash-between-turns case, yes — that's the honest edge. Scheduled migration (what we just ran) is lossless for everything except mid-turn state; the checkpoint architecture doc covers the plan. |

---

## Act 5 — Dead runners exit routing in seconds

**Opening line:** *"When a runner crashes, Kubernetes will notice — eventually.
Liveness probe, readiness probe, endpoint controller, kube-proxy iptables sync.
That's a lot of seconds in which k8s routes requests to a corpse. Watch what
we do instead."*

**What the audience should watch for:**
- The placement query uses a TTL: `last_heartbeat_ms > (time::now() - 90000)`.
  That's the router's guardrail, running in SurrealDB, independent of kube.
- We force-kill runner-0 and age its heartbeat out. The same query that
  returned 1 row now returns 0.
- A request sent to runner-1 for the agent that was on runner-0 does NOT
  get forwarded to a corpse — it clean-fails with HTTP 4xx.
- When runner-0 comes back (StatefulSet recreates it), it re-registers with
  a fresh heartbeat and is routable again.

**Expected pushback:**

| They say | You say |
|---|---|
| "This is what liveness probes are for." | Liveness probes are what kubelet uses to decide whether to restart the pod — they don't affect routing until the endpoint controller updates, and that has its own lag. We care about routing correctness, not restart policy. |
| "90 seconds is still a long time." | Configurable. And zero of those seconds have misrouted requests — the query excludes the stale runner immediately. What takes 90s is us *being certain* the runner is dead, not the time it takes to stop routing. |
| "Why not just catch connection refused and retry?" | Because connection refused means the data already left your pod. The router filters at the placement layer — the request never reaches the network for a dead destination. |

---

## Closing

**The line:**
*"You saw five things every other platform promises in roadmaps. They all
ran on a laptop in the last ten minutes. The CI for this repo runs all of
them on every commit."*

**If asked 'what's the gap?'**
- Mid-turn crash safety (Act 4 caveat) — planned, checkpoint architecture
  doc exists.
- Multi-cluster: today we assume one SurrealDB. Multi-region is a real
  conversation, not a demo fix.
- Model-agnostic session planning: today BAML is the planner; pluggable
  planners are a future thing.

Own these. The answer to "what can't this do yet" is more trustworthy than
pretending there are no gaps.

## Competitor quick-reference

| Claim you'll hear | Agentium's distinct position |
|---|---|
| "Bedrock Agents" | AWS-locked, no cross-provider, no portable provenance, no mid-turn migration. |
| "LangGraph + LangSmith" | Python library, no runtime isolation, logs not provenance, no cluster model. |
| "Temporal workflows" | Durable but deterministic-assumed; LLM activities need wrappers; no A2A transport. |
| "Istio + raw Lambda functions" | Service mesh without the agent model; you build A2A, provenance, lifecycle yourself. |
| "OpenAI Assistants API" | Chat wrapper; no cross-pod deployment, no self-hostable, no audit graph. |

The throughline: **everyone else has part of the stack. Agentium is the
stack.** And the demo is the test, which means the claims are testable.
