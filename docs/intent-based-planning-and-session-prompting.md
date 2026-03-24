# Intent-Based Planning and Session Prompting

**Start with** [How to write agents](how-to-write-agents.md) for onboarding (entrypoints, tools, plans + ReAct, citations). This document is the **deep guide** for plan-anchored prompting, session template ordering, and executor discipline—it complements that model (**ReAct with plans and revision**, not a separate “heavy” paradigm).

This guide defines best practices for intent-driven orchestration with host-managed session tools in this runtime.

It focuses on:

- Prompt design for high cache reuse.
- TypeScript orchestration structure for plan-anchored execution.
- Correct template ordering in session-calling prompts.
- Separation of responsibilities between runtime FSM, BAML prompts, and TS orchestration.

## Core Principles

- **Citations, not evidence strings:** planning intents and step transitions carry **ref-table `citations`**
  (`#N` history, `@N` archive) so the system has **citable history** and **checked citations** in
  provenance/drift — see `docs/citable-history-and-checked-citations.md`.
- Keep planning intent-focused, not FSM-mechanics-focused.
- Let runtime + generated session schemas enforce step shape and allowed operations.
- Use an append-only event log for session continuation context.
- Minimize prompt volatility before the model reaches the dynamic parts.
- Keep one reasoning locus: per-step `reason` only (avoid duplicated top-level reasoning fields).

## Plan-Anchored Execution Pattern

Use a strict sequence:

1. Infer intent from the user message.
2. Commit a discovery plan (capability matching).
3. Run discovery session hops to completion.
4. Build a structured execution plan (`plan_steps`, not `steps`).
5. Commit the execution plan.
6. Execute each delegated step via Step Executor hops.
7. Summarize final output for the user.

Why this works:

- Stable committed plan objects improve prompt prefix reuse.
- Session tools are executed as single-hop FSM fragments, which keeps each call deterministic.
- TS orchestration stays declarative (intent, plan, evidence), while runtime executes session mechanics.

## Prompt Responsibilities

### What prompts should do

- State the business goal in plain terms (for example: identify agents that satisfy intent).
- Provide stable output contract via `{{ ctx.output_format }}`.
- Provide compact dynamic inputs (`inferred_intent`, `allowed_ops`, latest event log).
- Avoid instructing internal runtime mechanics that are already enforced by schema/FSM.

### What prompts should not do

- Re-explain transition rules that runtime already enforces.
- Include stale status-token aliases or ad hoc FSM shorthand.
- Carry full user message after intent extraction in session continuation prompts.
- Include both `conversation_history` and session event log for the same Step Executor call.

## Session Prompt Template Ordering (Cache-Oriented)

For Step Executor prompts, prefer this ordering:

1. **Static goal block** (least volatile).
2. **`{{ ctx.output_format }}`** (stable contract text; high prefix reuse).
3. **Small dynamic control fields** (for example `inferred_intent`, `allowed_ops`).
4. **Event log tail** (most volatile, appended near the end).

Canonical skeleton:

```baml
prompt #"
  Goal: <business outcome, no FSM lecture>
  {{ ctx.output_format }}
  Inferred intent: {{ inferred_intent }}
  Allowed ops: {{ session_context.allowed_ops }}
  {% if ctx.tags['event_log'] %}
  Event log (most recent context):
  {% for event in ctx.tags['event_log'] %}
  - {{ event.role }} | {{ event.source }} | {{ event.content }}
  {% endfor %}
  {% endif %}
"#
```

Notes:

- `allowed_ops` must be present for step choice.
- Event log should be bounded/compacted upstream; only include most relevant recent context.
- Do not inject persona chat history into internal Step Executor prompts.

## Deriving Goal Text from Intent and Plan Steps

Do not keep `Goal:` as a generic placeholder. It should be derived from committed planning artifacts.

Use this mapping:

- Discovery Step Executor (`GetDiscoverAgentsPlan`):
  - Goal comes from inferred intent text.
  - Example: `Goal: identify candidate agents whose capabilities can satisfy this intent: <intent>.`
- Delegation Step Executor (`ExecutePlanStepWithDelegate`):
  - This does not build a plan. It solves the next execution hop for an already committed plan step.
  - Goal comes from committed step target + objective.
  - Example: `Goal: complete committed step <step_id> by obtaining the required output from <agent_package>/<agent_instance_id> toward objective: <objective>.`
- Plan Synthesizer (`MakeStructuredPlan`):
  - Goal comes from user outcome synthesis, not session mechanics.

Recommended helper shape in TS:

```ts
function discoveryGoal(intent: string): string {
  return `identify candidate agents whose capabilities can satisfy this intent: ${intent}`;
}

function delegationGoal(plan: StructuredPlan, step: PlanStep): string {
  const target = `${step.agent_package}/${step.agent_instance_id}`;
  return `execute delegated step for target ${target} toward objective: ${plan.objective}`;
}
```

Recommended prompt shape:

```baml
prompt #"
  Goal: {{ goal_text }}
  {{ ctx.output_format }}
  Inferred intent: {{ inferred_intent }}
  Allowed ops: {{ session_context.allowed_ops }}
  {% if ctx.tags['event_log'] %}
  Event log (most recent context):
  {% for event in ctx.tags['event_log'] %}
  - {{ event.role }} | {{ event.source }} | {{ event.content }}
  {% endfor %}
  {% endif %}
"#
```

Design note: if `goal_text` varies too much and hurts cache prefix reuse, keep a static `Goal:` sentence and move the specific target/intent to separate dynamic lines after `ctx.output_format`.

## Session Step Contract Guidance

Generated step schema descriptions should communicate:

- Emit exactly one FSM step.
- Choose operation from `allowed_ops`.
- After `Send`, prefer `Read` before more `Send`/`Finish` so tool output is consumed.

This belongs in generated output descriptions, not duplicated in every user-authored goal paragraph.

## TypeScript Orchestration Best Practices

Use TS to orchestrate intent/Plan Artifact lifecycle and evidence, not to reimplement the execution FSM.

### Recommended TS flow

- Call intent inference function once at start.
- Open execution session and submit intent + plan metadata.
- Call `runGeneratedStepExecutor(...)` for Step Executor functions.
- Treat Step Executor outputs as strict envelopes (`status`, `output`) and parse conservatively.
- On terminal status, consume final output and complete the corresponding execution step.

**Provenance note:** operator‑visible prose is **not** copied from step‑executor
`last`/`steps` into provenance as a second “reply.” The recorded user message is
the chat completion `SessionResult.message` (e.g. `StructuredReply` from a final
synthesis function). Step envelopes are execution evidence only.

## Iterative Plan-Step Solver (Reference Pattern)

An iterative solver should execute one committed step at a time, while each Execution Hop emits exactly one FSM step.

High-level loop:

1. Commit intent and Plan Artifact once.
2. For each committed `plan_step`:
   - Open execution evidence scope (`startStep`).
   - Run `runGeneratedStepExecutor("ExecutePlanStepWithDelegate", { plan, step }, ...)`.
   - Collect terminal output from strict envelope (`status === "done"`, read `output`).
   - Validate and record evidence (`completeStep`).
3. Finish execution session when all committed steps complete.

Reference pseudocode:

```ts
for (const step of plan.plan_steps) {
  await executable.startStep(step.step_id, `Starting ${step.step_id}`);

  const run = await runGeneratedStepExecutor(
    "ExecutePlanStepWithDelegate",
    { plan, step },
    { max_steps: 8 }
  );

  const terminal = [...run.steps].reverse().find(s => s?.status === "done") ?? run.last;
  const output = terminal?.output ?? null;
  if (output == null) {
    throw new Error(`Missing terminal output for ${step.step_id}`);
  }

  // Optional: normalize/merge streamed chunks here.
  await executable.completeStep(step.step_id, `Completed ${step.step_id}`);
}

await executable.finish();
```

Stop/failure criteria:

- Stop step loop only on terminal status with valid output envelope.
- Abort current plan if a committed step repeatedly fails envelope validation.
- Do not mutate committed step ordering mid-execution; replan as a separate explicit phase.

Why this is "iterative" but stable:

- Iteration is over committed plan steps, not ad hoc replanning on every hop.
- Execution hops remain local tactical decisions (`Open`/`Send`/`Read`/`Finish`), guided by `allowed_ops` and event log.
- Strategic intent and step ordering remain fixed for cache stability and auditability.

### TS do/don't

- Do: keep deterministic IDs (`intent-*`, `plan-*`, `step-*`) derived from semantic text.
- Do: maintain explicit execution evidence messages (`startStep`, `completeStep`, `finish`).
- Do: keep Step Executor calls narrow in input shape.
- Do not: handcraft session step JSON in TS.
- Do not: add shim-side policies that conflict with runtime FSM authority.
- Do not: pass raw user message to post-intent Step Executor calls unless specifically required.

## Runtime and Client Configuration

- Apply session-only model behavior (for example disabling reasoning-heavy mode) dynamically at runtime via per-call client registry overrides.
- Avoid introducing separate BAML clients solely for Step Executor calls when runtime can apply scoped overrides.
- Ensure pre-execution inspection and actual execution share the same effective client overrides.

## Event-Log Pattern (Backend Implementation)

This is primarily a Rust runtime/tooling concern, not a prompt-authoring trick.

### Runtime pipeline in this repo

- Conversation context provider in `baml-rt-a2a` reads recent store events.
- It calls `project_prompt_context(...)` from `baml-rt-tools/prompt_projection.rs`.
- `project_prompt_context(...)` applies per-tool compaction via `ToolHandler::compact_result(...)`.
- It emits three tags for prompts: `conversation_history`, `event_log`, and `session_state`.
- `baml-rt-quickjs` injects those tags into BAML `ctx.tags.*` for Step Executor calls.

### What to implement in a Rust tool

If your tool participates in session planning context, implement these pieces:

1. **Compact result payloads in the tool handler**
   - Override `ToolHandler::compact_result(&mut Value)`.
   - Remove bulky fields, keep only planner-relevant summaries/refs.
   - Never rely on prompt-side truncation as the primary strategy.

2. **Return session progress metadata**
   - Include `history_context: Option<HistoryContextV1>` in session outputs (`NextOutput`).
   - Populate `hop`, `op`, `status`, optional `cursor`, and compact `payload`.
   - Keep `payload` bounded and semantically useful for next-step selection.

3. **Define projection semantics when needed**
   - Use identity/summary/detail semantics for read paths where applicable.
   - Ensure summary mode stays token-cheap and detail mode is explicit/opt-in.

4. **Keep activity records source-tagged and ordered**
   - Preserve provenance `activity_anchor` (and any envelope `event_id` where that is the wire id), timestamp, role, and source.
   - Emit deterministic references instead of raw archive dumps when possible.

Minimal shape:

```rust
impl ToolHandler for MyTool {
    fn compact_result(&self, content: &mut serde_json::Value) {
        // Strip verbose arrays/raw blobs; preserve compact refs + key summary fields.
    }
}

#[derive(Serialize, Deserialize)]
struct MyNextOutput {
    // domain fields...
    history_context: Option<HistoryContextV1>,
}
```

### Why this matters for prompts

- Step Executor prompts should consume already-compacted `event_log` and `session_state`.
- Prompt templates remain simple and stable; backend owns payload shaping complexity.
- Cache reuse improves because volatile large payloads are normalized before prompt render.

## Review Checklist

- Goal text describes user/business outcome, not FSM machinery.
- `ctx.output_format` appears before highly volatile fields.
- Session prompt includes `allowed_ops` and event log tail.
- No `status_token`/legacy aliases in prompt inputs.
- No duplicated top-level and step-level reason fields.
- TS orchestration commits intent + Plan Artifact before execution.
- Runtime owns FSM transitions and anti-loop guards.

## Anti-Patterns to Avoid

- "Execute committed session by choosing transitions..." as the primary goal statement.
- Embedding transition micro-rules in user-authored prompt prose.
- Mixing conversational persona context into internal execution hops.
- Static hardcoded plan/intent IDs that bypass LLM-derived intent semantics.
- Shim-local compaction/heuristics that should live in Rust runtime/tooling.
