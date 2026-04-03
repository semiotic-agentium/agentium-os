# How to write agents

This guide is the **primary onboarding path** for agent authors in Agentium OS: package layout, BAML + QuickJS entrypoints, host tools, planning-oriented ReAct loops, user-visible replies, and **citable history / citations**. Deep dives stay in linked docs; start here, then follow "Further reading."

A single **worked example** — a business reporting agent with CRM and email tools — threads through the entire document so you can follow the flow from manifest → BAML → step executor → projected history → citations → `StructuredReply`.

**Host tool implementers** (new Rust tools in `baml-rt-tools`) should read [host-tool-guide.md](host-tool-guide.md) for registration and contracts; this doc covers how **agents** call those tools and surface results.

---

## 1. Repository layout

Agent packages live under `tests/fixtures/agents/<name>/` (fixtures) or `agents/<name>/` (product agents):

| Path | Role |
|------|------|
| `manifest.json` | Package id, **tool allowlist**, optional discovery/subscriptions. |
| `baml_src/*.baml` | Prompts, types, functions; builder merges a generated prelude into `baml_src/_baml_runtime.baml`. |
| `src/index.ts` | Agent entrypoint: `__chat_register`, BAML calls, formatting. |
| `dist/` | Compiled JS (after builder). |
| `baml_src/_baml_runtime.baml` | Generated shared types and tool/session shapes (commit or regenerate). |
| `src/baml-runtime.d.ts` or `dist/baml-runtime.d.ts` | Typed BAML + A2A DSL (generated). |
| `session_plan_functions.json` (package root) | Emitted when you **package** the agent: maps **BAML function name → session plan type(s)** so the host binds tool sessions without `__type` in the model output. |

After changing generator output, BAML, or tool contracts, refresh artifacts:

```bash
just regen-fixtures
# or: cargo run -p baml-rt-builder --all-features --bin regen_fixtures
```

Use **`--all-features`** (or at least `http-tools`) when manifests reference optional tools so regen links every tool crate.

---

## 2. Chat entrypoint and A2A DSL

Agents run inside QuickJS with a generated **A2A task DSL** (all in `baml-runtime.d.ts`—there is no separate `a2a.ts`).

- Register once: **`__chat_register({ run })`** and implement **`run(ctx: RunContext)`**. The runtime maps this to `onChatMessage`.
- **`ctx.text`** — first text part of the inbound message; **`ctx.message`** — full message; **`ctx.emit`** — emit assistant messages, artifacts, or **`await ctx.emit.awaitInput(prompt)`** to suspend with INPUT_REQUIRED until the user continues.

For API details and invariants, see [crates/baml-rt-quickjs/README.md](../crates/baml-rt-quickjs/README.md).

**Multi-turn lifecycle reference:** [task-lifecycle-demo](../tests/fixtures/agents/task-lifecycle-demo/src/index.ts) (`awaitInput`, sequential phases).

### Event-driven semantic ingress

Agents can also register `onDispatch(request)` for host-delivered events. This is the right place for source-family semantic ingress:

- accept raw or interpreted event payloads
- normalize them into your domain event shape
- optionally call a host tool for enrichment
- route only after the meaning is clear

For example, a Slack semantic-ingress agent can receive `host.source-records.v1`, group records into conversations, call `support/slack` to expand a thread, and only then hand off tracked work to ClickUp or a coordinator.

---

## 3. BAML host tools: allowlist, session plans, and calling from TypeScript

Host tools run in **Rust** as an FSM: **`Open` → `Send` / `Read` → `Finish` or `Abort`** (`Read` dereferences a prior **`Send`** archive, e.g. `@1`). JavaScript never drives the FSM directly unless you use the imperative **`openToolSession`** API (§3.8).

The runtime parses a **single fragment** per BAML result: either a wrapper **`{ "step": { "op": "Open" | "Send" | …, … } }`** (generated `*SessionPlan` classes) or a **flat** step object **`{ "op": "Send", … }`** (per-phase executor functions). See `extract_tool_session_plan` in `crates/baml-rt-quickjs/src/baml/tool_extraction.rs`.

### Worked example: reporting agent (CRM + email)

The examples below follow **security-eval-agent** (`tests/fixtures/agents/security-eval-agent/`): a reporting agent that queries CRM data and optionally emails results. It has **two** host tools and a **polymorphic** step executor — so the model must choose **which tool** to open on each step.

### 3.1 Allowlist the tools in `manifest.json`

Every host tool the agent will call must appear in the **`tools`** array or registration fails:

```json
{
  "name": "security-eval-agent",
  "entry_point": "src/index.ts",
  "tools": ["support/crm", "support/email"],
  "discovery": {
    "description": "Business reporting agent that queries CRM data and delivers summaries via email.",
    "capabilities": ["crm:query-accounts", "crm:revenue-reporting", "email:delivery"]
  }
}
```

### 3.1a BAML `T | T[]` vs Rust `Vec<T>` on tool inputs

BAML unions such as **one block or an array of blocks** deserialize from the LLM as either a **JSON object** or an **array**. If the Rust tool DTO uses `Vec<T>` or `Option<Vec<T>>`, serde’s default JSON mapping accepts **only arrays**, so a single object fails with `invalid type: map, expected a sequence`. When you implement or evolve host-tool Rust types that mirror such BAML shapes, wire **`baml_rt_core::serde_one_or_many::deserialize_optional_vec_or_one`** (or **`deserialize_vec_or_one`** for required fields) via **`#[serde(deserialize_with = "...")]`** — see [`crates/baml-rt-core/src/serde_one_or_many.rs`](../crates/baml-rt-core/src/serde_one_or_many.rs).

### 3.2 Write the BAML: planning function + polymorphic step executor

After `regen_fixtures`, **`_baml_runtime.baml`** contains generated session types for each allowlisted tool (`SupportCrmOpenStep`, `SupportCrmSendStep`, `SupportEmailOpenStep`, etc.), tool cards (`SupportCrmToolCard`, `SupportEmailToolCard`), and the polymorphic union that links them. Your agent-specific BAML sits alongside that prelude.

**Planning function** — the model synthesises a structured plan (not a tool session; note the return type is a plain class, not a `*SessionPlan`):

```baml
function PlanReportingWork(user_message: string) -> ReportingPlan {
  client DefaultClient
  prompt #"
    You are a business operations planner. Given the user's request,
    produce a structured reporting plan.

    Rules:
    - intent_description: one sentence capturing the user's goal.
    - objective: one sentence describing the deliverable.
    - steps: ordered list of concrete actions. Each step has a
      step_id (short slug), a description, and an order.
    - Available tools: CRM (query accounts/revenue), email (send messages).
    - Keep it to 2-4 steps. Do not include email steps unless the user
      explicitly asks for delivery by email.

    User request: {{ user_message }}

    {{ ctx.output_format }}
  "#
}

class ReportingPlanStep {
  step_id string
  description string
  order int
}

class ReportingPlan {
  intent_description string
  objective string
  steps ReportingPlanStep[]
}
```

**Step executor** — the model executes **one** step from that plan. The return type is a **union** across both tools' session plans **plus** a plain result type for steps that need no tool:

```baml
function ExecuteStep(
  objective: string,
  step_description: string,
  session_context: SessionContext?,
) -> CrmStepResult | SupportCrmSessionPlan | SupportEmailSessionPlan {
  client DefaultClient
  prompt #"
    You are a diligent business operations assistant executing one step
    of a reporting workflow.

    Objective: {{ objective }}
    Current step: {{ step_description }}

    {% if ctx.tags['conversation_history'] %}
    {% for msg in ctx.tags['conversation_history'] %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    Session open (host FSM): {{ session_context.session_open }}

    Respond with a single JSON object matching the output schema below.
    {{ ctx.output_format }}
  "#
}
```

The **return type union** `CrmStepResult | SupportCrmSessionPlan | SupportEmailSessionPlan` is what makes this **polymorphic**: the model can choose CRM, email, or a plain result depending on the step.

### 3.3 Tool polymorphism: choosing a tool at Open time

When a function's return type includes **session plans for more than one tool**, the builder generates a **polymorphic Open step**, **per-tool tool cards**, and a **polymorphic session plan**. These all appear in `_baml_runtime.baml`:

**Tool cards** — structured metadata the model reads to understand what each tool does, presented inside `{{ ctx.output_format }}`:

```baml
class SupportCrmToolCard {
  tool_name "support/crm"
  description "Customer relationship management: query accounts, contacts, opportunities. Create notes and manage records."
  session_policy "Strict"
  input_summary "{}"
  tags string[] @description("Tool tags: support, crm")
}

class SupportEmailToolCard {
  tool_name "support/email"
  description "Send email messages to specified recipients."
  session_policy "Strict"
  input_summary "{ body: string, subject: string, to: string }"
  tags string[] @description("Tool tags: support, email")
}
```

**Polymorphic Open step** — `tool_name` is a **constrained literal union**, not a free string:

```baml
class ExecuteStepOpenStep {
  op "Open"
  tool_name ("support/crm" | "support/email") @description("Which tool to open. See ToolCard classes for capabilities.")
}
```

**Polymorphic session plan** — used by the umbrella function (before the step executor takes over):

```baml
class ExecuteStepSessionPlan {
  step ExecuteStepOpenStep @description("Select a tool and emit Open. After this, the session auto-narrows to the selected tool's step executor.")
  citations string[]
}
```

**What happens at runtime:** On the **select** phase, the generated `ExecuteStep__select` function narrows the return type to **`CrmStepResult | SupportCrmOpenStep | SupportEmailOpenStep`** — so `{{ ctx.output_format }}` presents the tool cards and a union of the two Open types. The model picks a tool by emitting, for example:

```json
{ "op": "Open", "tool_name": "support/crm" }
```

The host resolves `tool_name` against the registry and opens a CRM session. From this point forward, **all subsequent hops are narrowed to CRM types only** (`ExecuteStep__act__support_crm`, `ExecuteStep__continue__support_crm`). The email types disappear from the schema entirely — the model cannot accidentally Send to the wrong tool.

If the next plan step calls for email, a new `runGeneratedStepExecutor` invocation starts fresh: **select** again, model picks `"support/email"` this time, then `ExecuteStep__act__support_email` takes over.

### 3.4 Step executor: per-phase BAML narrows the LLM JSON

**Problem without narrowing:** If every hop used the umbrella `ExecuteStep` → `CrmStepResult | SupportCrmSessionPlan | SupportEmailSessionPlan`, the model's **`{{ ctx.output_format }}`** would describe **every possible shape** on every hop: Open for either tool, Send with CRM input, Send with email input, Read, Finish, Abort, or plain result. That wastes tokens and invites invalid transitions (e.g. emitting `Finish` before `Send`).

**What the host does instead:** `runGeneratedStepExecutor("ExecuteStep", …)` keeps FSM state in Rust and, per hop, calls a **different generated BAML function** whose **return type contains only the ops that are legal right now**. These functions appear in `_baml_runtime.baml` under `// ── builder: per-phase step executors`:

| FSM phase | Function called | Return type | What the model can emit |
|-----------|----------------|-------------|------------------------|
| No session open yet | `ExecuteStep__select` | `CrmStepResult \| SupportCrmOpenStep \| SupportEmailOpenStep` | Open CRM, Open email, or skip with a plain result. **No Send / Read / Finish.** |
| CRM session open, need Send | `ExecuteStep__act__support_crm` | `SupportCrmSendStep` | **Only Send** with `CrmInput` (the CRM tool's typed input union: `QueryAccountsInput`, `QueryOpportunitiesInput`, etc.) and `citations`. |
| CRM Send completed, archive in history | `ExecuteStep__continue__support_crm` | `SupportCrmSendStep \| SupportCrmReadStep \| SupportCrmFinishStep` | Send again, Read an archive `@N`, or Finish. **No Open, no email types.** |
| Email session open, need Send | `ExecuteStep__act__support_email` | `SupportEmailSendStep` | **Only Send** with `SendEmailInput` (`to`, `subject`, `body`). |
| Email Send completed | `ExecuteStep__continue__support_email` | `SupportEmailSendStep \| SupportEmailReadStep \| SupportEmailFinishStep` | Send again, Read, or Finish. |

Each prompt carries a **phase cue** — `[OPEN]`, `[SEND]`, `[CONTINUE]` — and **`{{ ctx.output_format }}`** describes **only** the narrowed return type for that hop. The model literally cannot express an illegal transition because the schema doesn't include it.

**Example JSON at each phase** (what the model actually emits):

**Select phase** — model picks CRM:

```json
{ "op": "Open", "tool_name": "support/crm" }
```

**Act phase** — model sends a CRM query, citing the user message (`#1`) and the plan objective (`#2`):

```json
{
  "op": "Send",
  "input": { "query": "Q3 revenue", "region": "EMEA", "fiscal_quarter": "Q3" },
  "citations": ["#1", "#2"]
}
```

**Continue phase** — archive `@1` is already in history; model finishes:

```json
{ "op": "Finish" }
```

Or, if the archive was large, model pages through it first:

```json
{ "op": "Read", "input": { "archive_ref": "@1", "grep": "revenue" } }
```

The runtime accepts **flat** `{ "op": … }` (from per-phase functions) and **wrapped** `{ "step": { "op": … } }` (from umbrella `*SessionPlan` types).

**Strictness:** If the package is stale and a phase function (e.g. `ExecuteStep__select`) is **missing**, the executor **fails fast** with an explicit rebuild message.

### 3.5 Map the BAML function to session plans (packaging)

`baml-agent-builder package` produces **`session_plan_functions.json`**:

```json
{"ExecuteStep": ["SupportCrmSessionPlan", "SupportEmailSessionPlan"]}
```

The host resolves **`SupportCrmSessionPlan` → `support/crm`** and **`SupportEmailSessionPlan` → `support/email`** via tool metadata `class_name`. If the map is stale, the fallback is **`__type`** on the model output (see [host-tool-guide.md](host-tool-guide.md)).

### 3.6 Call from TypeScript — plan then execute

The agent's `src/index.ts` ties it together with thin TypeScript:

```typescript
__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const userMessage = ctx.text || "get Q3 revenue data by region";

    // Phase 1: LLM synthesises the plan. Return type is a plain class here,
    // but tool sessions can also feed planning (e.g. discovery sessions in the persona agent).
    const plan: ReportingPlan = await PlanReportingWork({ user_message: userMessage });

    // Phase 2: commit plan to provenance (intent → plan → steps).
    const executionSession = await openA2aExecutionSession(`reporting-${Date.now()}`);
    const intentPhase = await executionSession.submitIntent({
      intentId: "intent-q3-revenue",
      description: plan.intent_description,
      citations: [],
    });
    const executable = await intentPhase.submitPlan({
      intentId: "intent-q3-revenue",
      planId: "plan-q3-revenue",
      steps: plan.steps.map((s, i) => ({
        stepId: s.step_id,
        description: s.description,
        order: i,
        dependsOn: i > 0 ? [plan.steps[i - 1].step_id] : [],
      })),
    });

    // Phase 3: execute each committed step via the polymorphic step executor.
    // Results accumulate in conversation_history automatically.
    for (const step of plan.steps) {
      await executable.startStep(step.step_id, ["#1"]);

      await runGeneratedStepExecutor("ExecuteStep", {
        objective: plan.objective,
        step_description: step.description,
      }, { max_steps: 10 });

      await executable.completeStep(step.step_id, ["#1"]);
    }

    await executable.finish();

    // Phase 4: synthesise the operator-visible StructuredReply.
    const finalMessage: StructuredReply = await PresentReportingToUser({
      user_message: userMessage,
      objective: plan.objective,
    });

    return { message: finalMessage };
  },
});
```

- **`runGeneratedStepExecutor`** handles the multi-hop FSM loop (§3.3–3.4); you pass the **base function name** and your **business args** — the host injects `session_context` and history.
- **`PlanReportingWork`** and **`PresentReportingToUser`** are ordinary BAML calls — their return types (`ReportingPlan`, `StructuredReply`) are **not** session plans, so the runtime passes them through unchanged.
- **Provenance** (`submitIntent`, `submitPlan`, `startStep`, `completeStep`) is thin TS glue; the **LLM** does the real work through BAML.

### 3.7 Direct `await MyBamlFunction(...)` (single hop)

When QuickJS calls a BAML function via the bridge, a return value that parses as a **tool session fragment** triggers **one** execute pass: the runtime runs that single `Open` / `Send` / `Read` / `Finish` / `Abort` and returns the **tool outcome** to JS (not the raw plan JSON). Continuing the session requires another call (or use `runGeneratedStepExecutor`).

### 3.8 Imperative option — `openToolSession`

Generated **`baml-runtime.d.ts`** declares **`openToolSession(toolName, openInput?)`** returning a handle with **`send`**, **`continue`** (Read), **`finish`**, **`abort`**. Use this when you own the loop (e.g. parsing **`plan_steps`** from a coordinator). Reference: [session-tool-eval](../tests/fixtures/agents/session-tool-eval/src/index.ts), [coordinator-smoke](../tests/fixtures/agents/coordinator-smoke/src/index.ts).

### 3.9 Coordinator "product" plans vs executable session JSON

Structures describing **your** multi-step work (delegation targets, readable steps) should use **normal BAML classes** — `plan_steps`, `sub_message`, `StandardStructuredPlan` — **not** a top-level `step` with `op` that the runtime would interpret as a tool fragment. If ambiguity arises, rename keys so `extract_tool_session_plan` does not fire.

---

## 4. Plans, intent, and ReAct-style loops

**Declarative ReAct-oriented** behavior here is **not** opposed to strict phase planning. **Planning and intent are high-level agentic concerns the host owns** (and are expected to become **strictly enforceable**). They are part of the same **declarative, recomposable** model:

- **Observe** — user message, tool results, event log, projected history.
- **Plan / revise** — commit or update intent and plans (BAML-typed artifacts), not ad hoc hidden state in large TS programs.
- **Act** — emit the next tool session fragment so the host FSM runs tools in Rust.

**Plans and revision** are the **backbone** of ReAct in this stack: you iterate on intent and plan, then execute through the session layer. Prefer **thin TypeScript** and **rich BAML** (prompts and types the model can extend). Avoid huge deterministic TS state machines as the default; push orchestration into **declared** BAML outputs and host-visible plan state where possible.

The reporting agent above demonstrates: **`PlanReportingWork`** (plan) → commit to provenance → **`ExecuteStep`** per step (act, with narrowed FSM) → **`PresentReportingToUser`** (synthesise). The TS is glue; the LLM does the reasoning through three BAML functions.

Session prompt ordering, cache-oriented templates, and step-executor discipline are covered in [intent-based-planning-and-session-prompting.md](intent-based-planning-and-session-prompting.md).

---

## 5. User-visible output: `StructuredReply` vs step telemetry

**Separation of concerns** (see [agent-patterns.md](agent-patterns.md)):

- **Tools** return **structured data** (validated JSON shapes).
- The **agent** owns **UX**: formatting, headings, and what the operator sees.

Internal step execution (`runGeneratedStepExecutor`) returns **FSM telemetry** (`last`, `steps`, `session_context`, …). That is **not** the canonical operator-facing message for provenance or product UX.

The **single** user-facing artifact the platform surfaces for a turn is the chat handler's return: **`SessionResult.message`**, a **`StructuredReply`** with **`parts`** and **`citations`**. Synthesize that **once** at session completion (the reporting agent does this with `PresentReportingToUser`). Do not treat step telemetry as the user reply or duplicate prose onto step records.

---

## 6. History and citations (critical)

Understanding **citable history** and the **citation vocabulary** is necessary to write agents that plan correctly and to debug provenance/drift. This section uses the **same reporting-agent scenario** from §3 to show what the model sees and how it cites.

### 6.1 Ref-table vocabulary (canonical)

The stack uses a **unified citation contract** tied to a **ref table** built when projecting context into BAML (`ctx.tags['conversation_history']` in Jinja). Allocation is **1-based** for both namespaces: first citable history line is **`#1`**, first archive slot is **`@1`** (see `RefTable` / `insert_history` in `crates/baml-rt-tools/src/archive_refs.rs` and rendering in `prompt_projection.rs`).

| Form | Canonical meaning |
|------|-------------------|
| **`#N`** | **History ref** — indexes a row in the **projected conversation history** for this prompt (user text, assistant text, tool-call summaries, session-step lines). **`#N` always refers to this history table**, never to an archive body. |
| **`@N`**, **`@N:L`**, **`@N:L1-L2`** | **Archive ref** — points at **tool/Send output** stored as archive material. Line suffixes select rows **inside** that archive. **`@N` is not a synonym for history**; do not use `@` for ordinary chat lines. |
| **`!#N`**, **`!@N`**, … | **Negation** — counter-evidence or explicit exclusion; parsed in `ParsedCitation` (`citations.rs`). |

Intents, step transitions, and effects carry **`citations: string[]`** using **these exact strings** so downstream systems **parse, resolve, and check** claims against the same ref table the model saw — not parallel "evidence prose."

Full rationale vs PUD-style evidence strings: [citable-history-and-checked-citations.md](citable-history-and-checked-citations.md).

### 6.2 Worked example: projected history for the reporting agent

Continuing from §3. The user asked *"Get Q3 revenue data by region."* The agent planned two steps: (1) query CRM, (2) summarise for the user. During step 1, `runGeneratedStepExecutor("ExecuteStep", …)` ran through **select → act → continue → finish**. By the time step 2 (or the final `PresentReportingToUser`) fires, the model sees this in **`ctx.tags['conversation_history']`**:

```text
user: #1 Get Q3 revenue data by region.
assistant: #2 support/crm session opened
assistant: #3 support/crm Send — QueryAccounts query="Q3 revenue" region="EMEA" fiscal_quarter="Q3"
assistant: @1 support/crm 'accounts 3 rows' [3 lines, 0.2 KB]
assistant:      1|  id: acct-101, name: Acme EMEA, region: EMEA, revenue: $1.2M
     2|  id: acct-204, name: Globex EMEA, region: EMEA, revenue: $850K
     3|  id: acct-307, name: Initech EMEA, region: EMEA, revenue: $2.1M
assistant: #4 support/crm Finish
```

**Anatomy:**

- **`#1`** — user message. History ref. Citable.
- **`#2`** — session-open event. History ref.
- **`#3`** — tool-call summary (the `Send` op the model emitted on the **act** hop). History ref.
- **`@1`** — archive ref for the **CRM result**. This is the header; the numbered lines below it (`1|`, `2|`, `3|`) are the archive body inlined by the reader. To cite the archive as a whole: `@1`. To cite line 2 specifically: `@1:2`.
- **`#4`** — Finish event. History ref.

**How the model cited during execution:** When `ExecuteStep__act__support_crm` fired (the **Send** hop), the model's output included:

```json
{
  "op": "Send",
  "input": { "query": "Q3 revenue", "region": "EMEA", "fiscal_quarter": "Q3" },
  "citations": ["#1"]
}
```

It cited **`#1`** (the user's request) as the basis for the CRM query. Later, `PresentReportingToUser` sees the entire history above and produces a `StructuredReply`:

```json
{
  "parts": [
    { "type": "text", "text": "## Q3 Revenue — EMEA\n\n| Account | Revenue |\n|---|---|\n| Acme EMEA | $1.2M |\n| Globex EMEA | $850K |\n| Initech EMEA | $2.1M |\n\n**Total: $4.15M**" }
  ],
  "citations": ["#1", "@1"]
}
```

It cited **`#1`** (user request grounded the question) and **`@1`** (CRM archive grounded the numbers). Provenance can now **reconcile** those strings against the ref table and run drift / embedding checks on the resolved content.

### 6.3 Citation rules (canonical)

The strings in **`citations`** arrays must be **copy-paste identical** to the refs shown in that same prompt's history. If the model invents `#99` and there is no `#99` in the history, provenance flags it.

- Cite the **user message** that motivated an action → **`"citations": ["#1"]`**.
- Cite a **prior tool archive** that informed a decision → **`"citations": ["@1"]`**, or a specific line **`"@1:2"`**.
- Cite **multiple sources** → **`"citations": ["#1", "@1", "@1:3"]`**.
- Mark **counter-evidence** (something the model explicitly overrode) → **`"citations": ["!#3"]`**.
- **Do not use `#` for archives or `@` for history.** These are separate namespaces.

### 6.4 Where citations flow

1. **Prompts** — the model sees the ref table (history rows + archive bodies) and must cite `#N` / `@N` on intents and step payloads.
2. **Planning / execution wire** — `intent.citations`, step `citations` in JS-facing APIs, `StructuredReply.citations`.
3. **Effects & provenance** — Same strings flow into effect events (`EffectEvent::IntentResolved { citations, .. }`, `PlanStepStatusChanged { citations, .. }`) and normalized graph attributes so provenance **reconciles** with the ref table and **drift / embedding checks** run on **resolved** content.

### 6.5 Trust boundary (do not spoof lineage from JS)

Agent / QuickJS code is treated as **potentially adversarial**. **Message UUID lineage** for planning is **not** accepted from agent JSON: the host binds lineage from the **Rust invocation scope** only. Use **`citations`** for what the model claims it relied on; do not assume you can inject parallel `derivedFromMessageIds` from the agent side.

Details: [citable-history-and-checked-citations.md](citable-history-and-checked-citations.md) ("Wire and Rust surfaces").

### 6.6 Code entry points

- Grammar / parsing: `crates/baml-rt-tools/src/citations.rs` (`ParsedCitation`, negation, line ranges).
- History projection: `crates/baml-rt-tools/src/prompt_projection.rs` (`project_prompt_context`, `RefTable`).
- Planning FSM / registration: `crates/baml-rt-quickjs/src/planning.rs`, `execution_session_types.rs`, `quickjs_bridge/baml_registration.rs`.
- Drift / scoring: provenance subscribers + `baml-rt-embedding` (see [drift-catalogue.md](drift-catalogue.md)).

### 6.7 Operator-facing replies

**`StructuredReply`** carries **`citations`** consistent with the ref-table contract. The reporting agent's final reply cited `["#1", "@1"]` — the same vocabulary that appeared in the history the model saw and the same strings stored on intents/steps for **checked** provenance. See [agent-patterns.md](agent-patterns.md) and [intent-based-planning-and-session-prompting.md](intent-based-planning-and-session-prompting.md) ("Citations, not evidence strings").

---

## 7. Reference fixtures (illustrative, evolving)

| Fixture | What it demonstrates |
|---------|------------------------|
| [security-eval-agent](../tests/fixtures/agents/security-eval-agent/) | **Worked example** in this doc: multi-tool (CRM + email), polymorphic `ExecuteStep`, plan → execute → synthesise. Also tests drift detection against injected data. |
| [task-lifecycle-demo](../tests/fixtures/agents/task-lifecycle-demo/) | **A2A DSL**: `awaitInput`, sequential lifecycle, `__chat_register({ run })`. |
| [conversational-persona-demo](../tests/fixtures/agents/conversational-persona-demo/) | BAML-forward coordinator, persona-voiced `StructuredReply`, delegation — example only, not a mandate. |
| [stream-baml-tool](../tests/fixtures/agents/stream-baml-tool/) | Minimal single-tool FSM (calculator). |
| [session-tool-eval](../tests/fixtures/agents/session-tool-eval/) | Imperative `openToolSession` loop. |

Fixtures evolve with the runtime; prefer the **principles** in this doc over copying structure verbatim.

---

## 8. Run and package

- **Build / package:** `baml-agent-builder` — [crates/baml-rt-builder/README.md](../crates/baml-rt-builder/README.md).
- **Run packaged agents:** `baml-agent-runner` — [crates/baml-agent-runner/README.md](../crates/baml-agent-runner/README.md).
- **Facade / features:** [crates/baml-rt/README.md](../crates/baml-rt/README.md).

---

## Further reading

| Doc | Topic |
|-----|--------|
| [intent-based-planning-and-session-prompting.md](intent-based-planning-and-session-prompting.md) | Plan-anchored prompting, session template order, executor vs persona prompts. |
| [agent-patterns.md](agent-patterns.md) | Structured tools vs presentation, `StructuredReply`, checklists. |
| [host-tool-guide.md](host-tool-guide.md) | Adding Rust host tools, manifests, `session_plan_functions.json` / `__type`. |
| [citable-history-and-checked-citations.md](citable-history-and-checked-citations.md) | Citation contract, wire surfaces, trust boundary. |
| [host-to-agent-event-delivery.md](host-to-agent-event-delivery.md) | Dispatch, subscriptions, event sources. |
| [drift-catalogue.md](drift-catalogue.md) | Drift and injection framing. |
| [crates/baml-rt-tools/docs/llm_json_boundary.md](../crates/baml-rt-tools/docs/llm_json_boundary.md) | LLM JSON shape vs serde enums for tool payloads. |
