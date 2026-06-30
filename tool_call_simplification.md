# Tool call simplification evaluation

Evaluation of whether agents should expose a simplified tool-call surface (e.g. `Send(slack-notify, …)` only) versus the current host FSM: **Open → Send → SearchRead/PageRead → Finish/Abort**.

Scope: internal host tools, MCP tools, and external tools (e.g. `crates/tools/slack-notify/`). This document evaluates trade-offs and implementation impact only — no code changes proposed here.

---

## Verdict

**Hiding Open/Finish from the LLM while keeping the host FSM underneath is not a regression.** That aligns with capabilities the runtime already has (`ToolRegistry::execute()`, strict auto-open on Send fragments).

**Removing the FSM entirely would be a regression.** It would break archive reads, multi-tool routing, external/MCP session lifecycles, and provenance invariants described in the runtime thesis.

The simplification is best understood not as a per-tool exemption but as a **contract principle**: the LLM emits _intent_, the runtime owns _lifecycle_. See the next section. Most of the remaining ceremony lives in the **step-executor / BAML contract**, not in the tool implementation.

---

## Core principle: intent ops vs lifecycle ops

The goal is to **offload session-state management from the model**. The model wants to get information or cause an effect by calling a tool. It should not have to reason about "open first, then send, and don't send again before finishing." LLMs are not good at that bookkeeping; the runtime is. Provenance completeness stays an invariant regardless.

Every op in the FSM falls into one of two categories:

| Category      | Ops                                                                                    | Who emits   | Visible to LLM? | Why                                              |
| ------------- | -------------------------------------------------------------------------------------- | ----------- | --------------- | ------------------------------------------------ |
| **Intent**    | `Send` (perform effect / issue query), `Read @N` (materialize a specific archive page) | **LLM**     | **Yes**         | This is _what the agent wants_ — irreducible     |
| **Lifecycle** | `Open`, `Finish`, abort-on-error, double-send guard, session reuse                     | **Runtime** | **No**          | This is _bookkeeping_ — the runtime can infer it |

The headline win — **stateless resend**: once the runtime auto-finishes after each `Send → Done` (OneShot + Strict), every `Send` is its own fresh session. The model never tracks "am I in an open session?" or "must I Finish before sending again?" Each intent stands alone. The entire class of `Tool session already has input` / `Open before Send` / `Finish before Send` errors **disappears from the model's surface**.

This reframes the older "one-shot vs investigative" split. It was never about the tool being write-only. It is about **which ops are intent and which are bookkeeping**:

- Investigative / MCP tools keep `Read @N` visible because **reads are intent** — the agent is choosing which evidence to materialize — not because their lifecycle is special.
- One-shot tools simply have no intent beyond a single `Send`, so nothing but `Send` should surface.

Applied consistently, lifecycle is hidden for **all** tool classes — multi-tool, MCP, even MultiSend coordinators (with one open design decision called out in [MultiSend](#multisend-the-one-open-decision) below). The only op that _legitimately_ surfaces as something other than Send/Read is `Open` **when it carries required configuration the model must choose** (non-empty `open_input`).

---

## Invariant: internal FSM and provenance stay; only the LLM contract shrinks

This proposal **does not** remove Open, Send, Read, or Finish from:

- the **host session protocol** (`ToolSessionOp`, `open_session` / `session_send` / `session_read` / `session_finish`)
- the **provenance graph** (distinct Open / Send / SearchRead / PageRead / Finish activities)
- **external/MCP adapters** (`tool/session_open`, `session_send`, `session_finish`, etc.)

Those remain **runtime invariants**. Simplification means the model emits fewer ops for eligible tools; the host **still performs the full lifecycle internally** and records the same graph nodes (often with synthetic reasons such as `auto-open for send fragment with no open session`).

| Layer                                    | Simplification                                                                    | Unchanged                      |
| ---------------------------------------- | --------------------------------------------------------------------------------- | ------------------------------ |
| **LLM-visible step**                     | Intent ops only (`Send`, `Read @N`); lifecycle hidden                             | —                              |
| **Host execution**                       | Inserts Open before Send when needed; auto-finishes after Send→Done for one-shots | Full FSM                       |
| **Provenance / conversation projection** | Same event kinds and ordering                                                     | Open → Send → (reads) → Finish |
| **Investigative / streaming tools**      | Reads stay first-class; lifecycle still hidden                                    | Full FSM underneath            |

The full-semantics rewrite ("function-call semantics everywhere", [Tier 3](#tier-3--full-tool-calls-only-semantics-rejected)) is rejected because it would **collapse** this internal model. The accepted path only **hides bookkeeping from the model**, not from the runtime.

### `Send` (execute) is the policy gate — never fuse it with output

`Send` is not bookkeeping. It is the **provenance + security marker**: the recorded "I did this", and the point where an authorization/policy check can **block the effect before it commits**. Collapsing lifecycle must never collapse this gate.

- The blocking / authz hook fires on the **`Send` sub-step**, _before_ the output read materializes — **even when** auto-open + send + internal-read + auto-finish run in one host call (`ToolRegistry::execute()`).
- `Send` stays a **distinct, typed, recorded** graph node; it is never fused with the read/output node.
- This is precisely why only **lifecycle** (Open/Finish) is elided and **intent** (Send, `@N` reads) is not: eliding `Send` would erase the one hook that can refuse an effect, and conflating it with output would leave nothing to gate on.

---

## Current model

Host tools run in Rust as a session FSM:

```
Open → Send → SearchRead/PageRead → Send → … → Finish/Abort
```

Constraints enforced today:

- There cannot be more than one `Send` per hop under `SessionPolicy::Strict` (default).
- `Send` before `Open` is rejected (except strict auto-open on the single-fragment path — see below).
- JavaScript never drives the FSM directly unless using the imperative `openToolSession` API.

The step executor narrows what the LLM can emit per hop via generated phase functions (`__entry`, `__active__*`), documented in [`how-to-write-agents.md`](assertions/how-to-write-agents.md) §3.3–3.4.

### Example: slack-notify today

For `support/slack_notify` (`OpenInput = ()`, `SessionPolicy::Strict`, write-only, one-shot), the model typically pays **three LLM hops**:

1. **Entry:** `{ "op": "Open", "tool_name": "support/slack_notify" }`
2. **Active:** `{ "op": "Send", "input": { "text": "...", "context_id": "..." } }`
3. **Active:** `{ "op": "Finish" }`

Rust could already run `open → send → read → finish` in one host call via `ToolRegistry::execute()` for `ToolCapability::OneShot` tools.

---

## What the current design is buying you

The Open → Send → Read\* → Finish model is not arbitrary. It encodes runtime invariants from the [runtime thesis](assertions/agentium-runtime-thesis.md) and [host-tool guide](reference/host-tool-guide.md).

| Concern                     | Why Open/Send/Finish exist                                                     |
| --------------------------- | ------------------------------------------------------------------------------ |
| **Host-mediated effects**   | Every hop is a provenance event, not an opaque RPC                             |
| **Archive model**           | `Send` creates `@N` archives; `SearchRead`/`PageRead` materialize them         |
| **Phase narrowing**         | `__entry` vs `__active__*` unions stop invalid transitions at the schema level |
| **Polymorphic tool choice** | The model picks among tools; the runtime must know which one                   |
| **External/MCP lifecycle**  | Real `session_open` / sandbox checkout / `session_finish` map to Open/Finish   |
| **SessionPolicy**           | `Strict` vs `MultiSend` prevents double-send bugs after output is pending      |
| **Streaming tools**         | `ToolCapability::Streaming` needs an open session and multiple reads           |

None of these require the _model_ to emit lifecycle ops — they require the _runtime_ to run the lifecycle. The principle above keeps every concern, moving only the bookkeeping off the model.

---

## What already exists toward simplification

The runtime is not starting from zero.

| Mechanism                      | Location                                                                                                           | What it does                                                                                                                                                                                                         |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`ToolRegistry::execute()`**  | `crates/baml-rt-tools/src/tools.rs` (gates on `capability() != OneShot`)                                           | One-shot convenience: `open → send → read → finish` for `ToolCapability::OneShot`                                                                                                                                    |
| **Strict auto-open**           | `crates/baml-rt-quickjs/src/baml/tool_session_plan.rs` (reason `auto-open for send fragment with no open session`) | If the LLM emits only `Send` (single-fragment path) and `open_input` is empty, the runtime inserts `Open` automatically                                                                                              |
| **Typed per-tool steps**       | builder codegen (`SupportCalculateSendStep`, `…OpenStep`, `…FinishStep`)                                           | Each tool gets its own named step type; tool identity is carried by the type, resolved by constrained decode                                                                                                         |
| **`ReadOnlyFinish`**           | Step executor entry hop                                                                                            | Shortcut when no tool invocation is needed                                                                                                                                                                           |
| **Phase narrowing**            | `run_step_executor_loop` + builder codegen                                                                         | Model never sees the full op union at once; reduces invalid transitions                                                                                                                                              |
| **`SessionPolicy::MultiSend`** | `crates/baml-rt-tools/src/tools.rs`                                                                                | Opt-in for tools that genuinely need multiple Sends before reads                                                                                                                                                     |
| **Graph-only events**          | conversation projection                                                                                            | `SendDone` is recorded in the graph and flows through projection (replay / ref-tables) but is **not surfaced as a model/user-facing history line** — precedent that graph-truth ≠ what the model sees already exists |

Open-input emptiness for auto-open is shared between builder codegen and runtime via `open_input_schema.rs` (`schema_allows_empty_or_optional_open_input`).

### What is not there yet

- **Auto-finish** in the step-executor loop after `Send → Done` for Strict + OneShot tools
- **`Send` on the entry hop** (typed per tool) when the tool's `open_input` is empty, so the model never emits `Open` — a **builder change** (entry union) **and** a **dispatch change** (resolve tool from `<Tool>SendStep`) **and** an **entry-FSM change** (accept Send→terminate instead of requiring `status: open`)
- An **inferred auto-finish rule** — close after `Send → Done` when `capability == OneShot` + `Strict` + no `@N` read pending — rather than a new policy enum; the principle makes this derivable from existing metadata
- An **intent-level error contract** so failures never surface in lifecycle terms (see [Error / recovery contract](#error--recovery-contract))

---

## Multi-tool is the default case

Most real agents call several tools (CRM + Slack + Grafana MCP + …), not one. Multi-tool is **not** an edge case, and the principle above does not assume single-tool agents.

The typed-step pattern for multi-tool **already exists** — the builder emits a **typed `<Tool>SendStep` per tool** (`catalog_rendering_test.rs` shows `SupportCalculateSendStep` etc.), used today on the **active** phase and in the catalog. What does **not** exist yet is using it on **entry**: entry codegen currently emits `<Tool>OpenStep` only (`session_from_ir/mod.rs:231-236`). Tier 1's job is to **extend the entry union** to include typed Sends for eligible tools:

- A polymorphic entry hop becomes a **union of typed Sends**: `GrafanaPromSendStep | GrafanaLokiSendStep | SupportSlackNotifySendStep | …` (plus archive reads / `ReadOnlyFinish`).
- **Tool identity is the step type**, picked by constrained decode — not a free-text `tool_name` field, and not a remembered "I opened X earlier."
- This needs a **dispatch change too**: today resolution runs via `selected_tool` / `tool_name_for_scope` / `SessionPlan` suffix (`tool_invocation_plan.rs`), none of which derive the tool from a `<Tool>SendStep` class name on entry. Tier 1 adds that path (`FooSendStep → Foo → registry metadata`), symmetric to how `<Tool>OpenStep` already encodes its tool.
- Once resolved, the runtime auto-opens (empty `open_input`), sends, reads internally, and auto-finishes.

So for, e.g., 11 independent evidence queries across Prometheus / Loki / annotations in one observability agent: **11 typed Sends + 1 trailing `ReadOnlyFinish` = 12 model hops, not 44 FSM hops** — even though the agent is multi-tool. (The per-query Open/Finish lifecycle ops collapse; the single reply hop stays.) (Optional `@N` reads can add hops when a Send's archive header isn't enough — but those are _intent_, not lifecycle bookkeeping, so they are not the overhead we are cutting.) Picking a typed variant under constrained generation is more reliable for small models than emitting a correct tool-name string, which is why this — not a unified `Invoke` — is the recommended path. (See [Deferred: Invoke](#deferred-invoke-step-parked).)

---

## How the backend knows which tool to open (Send-only and reads)

When the model emits only `{ "op": "Send", … }` (via a typed `<Tool>SendStep`, no explicit Open), tool identity comes from **phase narrowing + the step type + manifest metadata**, then the runtime auto-opens if `open_input` is empty.

### Resolving `tool_name` before auto-open

| Situation                              | How the host picks the tool                                                                                                                                                                                                         |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Single-tool step executor**          | `session_plan_functions.json` lists one candidate; `resolve_baml_tool_invocation_plan` maps the invoking BAML function → that tool **without** `tool_name` on the step (`crates/baml-rt-quickjs/src/baml/tool_invocation_plan.rs`). |
| **Polymorphic entry (multiple tools)** | The chosen **typed `<Tool>SendStep`** pins the tool. No `tool_name` field and no prior Open required — the type _is_ the disambiguator.                                                                                             |
| **Active phase after a prior Open**    | Step loop binds `ToolBinding { name, slug }`; the return type is that tool's `*SendStep` only — the BAML type system pins the tool.                                                                                                 |
| **Send/Read after Open (legacy path)** | `selected_tool` is omitted on the step; runtime recovers the tool from the **live session row** for the scope (`tool_name_for_scope`).                                                                                              |

Once `tool_name` is known, strict auto-open inserts `Open { initial_input: None }` before `Send` when no session exists and `open_input` is empty/optional. Provenance still records Open, then Send, then Finish (when auto-finish is added).

**Example (slack-notify):** the model emits `SupportSlackNotifySendStep`; the host resolves `support/slack_notify` from the step type, auto-opens, sends, internally reads once, auto-finishes — **one Send hop** (no Open/Finish), full graph underneath. The turn still ends with a trailing `ReadOnlyFinish` reply hop, so the model pays **2 hops total, down from 3** (the lifecycle ops Open/Finish are what collapse, not the reply hop).

### Reads: what stays visible vs what the host does silently

| Kind                                   | Who emits it                                     | Role                                                                                                |
| -------------------------------------- | ------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| **Internal `session_read`** after Send | Host only (inside `execute()` / send+read cycle) | Drains the tool session FSM (`Done` / `Streaming`); not an LLM hop. Always runs; unchanged.         |
| **`SearchRead` / `PageRead` on `@N`**  | Model (optional)                                 | Materializes archive pages from a prior Send's `@N` header. **Stays first-class** — it is _intent_. |

**Archive reads without a tool Open:** global `@N` steps (`ArchiveSearchReadStep` / `ArchivePageReadStep`) route via `ArchiveRead` in `tool_invocation_plan.rs`; they target a visible archive ref, not a tool session, so they need no `tool_name`.

### Metadata signals (oneshot vs session)

- **External tools:** `tool-manifest.json` → `invocation_mode` (`single_shot` \| `session`) and `session_policy` (`strict` \| `multi_send`).
- **Host tools:** no per-tool JSON manifest; `session_policy` is on `ToolFunctionMetadata` / tool cards; one-shot semantics come from `ToolHandler::capability()` (`ToolCapability::OneShot`, default for `BamlTool`).
- **Builder / LLM catalog today:** exposes `session_policy`, not oneshot — Tier 1 should thread `invocation_mode` / `capability` into exported metadata so entry-hop Send and auto-finish scoping match runtime behavior.

---

## Decision table: LLM surface per tool shape

The complete contract in one table. Inputs: `capability` × `session_policy` × whether `open_input` is empty.

| Tool shape                                                                                | Entry hop the model sees                     | Active hops the model sees | Who finishes                                                                                                                             |
| ----------------------------------------------------------------------------------------- | -------------------------------------------- | -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| **OneShot, Strict, `open_input` empty** (slack-notify, most internal tools)               | typed `<Tool>SendStep` (runtime auto-opens)  | optional `@N` read         | **Runtime**, after `Send → Done`                                                                                                         |
| **OneShot, Strict, `open_input` required**                                                | `<Tool>OpenStep` carrying config → then Send | optional `@N` read         | **Runtime**, after `Send → Done`                                                                                                         |
| **Independent sends, `open_input` empty** (observability checklist: many one-off queries) | typed `<Tool>SendStep` (runtime auto-opens)  | `@N` reads                 | **Runtime**, auto-finish per `Send → Done` (stateless resend)                                                                            |
| **True MultiSend coordinator, `open_input` empty** (sends accumulate before a read)       | typed `<Tool>SendStep` (runtime auto-opens)  | more `Send` + `@N` reads   | **Runtime**, per the explicit MultiSend choice (see [MultiSend](#multisend-the-one-open-decision)) — _not_ an implicit "scope ends" rule |
| **Streaming, `open_input` empty**                                                         | typed `<Tool>SendStep` (runtime auto-opens)  | repeated `@N` reads        | **Runtime**, on stream `Done` — **never after the first read**                                                                           |

Rule of thumb: `Open` surfaces to the model **only** when it carries required configuration the model must choose (non-empty `open_input`) — this is the **single legitimate model-visible lifecycle op**. In every other case the runtime owns Open and Finish.

### MultiSend: the one open decision

MultiSend coordinators are the one case the principle does not resolve mechanically. There are two viable contracts; **this needs a deliberate choice, not a default**:

1. **Runtime-coalesced (preferred where possible):** the model emits N independent `Send` intents; the runtime decides whether they share one session or open fresh each time. The model stays fully stateless. Works when sends are independent (the common observability case).
2. **Explicit batch-then-read:** when an agent genuinely needs multiple Sends to accumulate state _before_ a single read (true coordinator semantics), the model expresses that as a sequence of Sends within one scope. Here the multi-send _is_ intent, so it stays visible — but `Open`/`Finish` are still runtime-owned.

Do not punt this to "semantics may matter." Pick per coordinator based on whether the sends are independent (→ option 1) or accumulate (→ option 2).

---

## Error / recovery contract

Offloading lifecycle to the runtime means the runtime also owns **failure handling**. Otherwise the happy path is clean but the error path leaks FSM concepts back to the model — defeating the purpose.

Invariant: **the LLM never sees an error phrased in lifecycle terms.** No "open before send", no "finish before send", no "tool session already has input". Errors are always intent-level: _this call failed, here is why._

| Failure                                            | Runtime behavior                                                            | What the model sees                                                                                                        |
| -------------------------------------------------- | --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| **Auto-open fails** (tool unavailable, bad config) | Runtime aborts the synthetic open                                           | A failed `Send` result in the transcript with the underlying reason — the model can retry as a new Send or route elsewhere |
| **Send errors mid-session**                        | Runtime aborts the session (lifecycle) so nothing dangles into the next hop | A failed `Send` result; the next hop starts clean (no leaked open session)                                                 |
| **Double-send under Strict**                       | Cannot occur: each Send is a fresh auto-opened/auto-finished session        | Nothing — the error class is gone from the surface                                                                         |
| **Double-send under MultiSend**                    | Runtime, not the model, tracks send count and policy                        | Nothing — policy is enforced host-side                                                                                     |
| **Stream errors mid-read**                         | Runtime aborts the session, marks the archive partial                       | A read result flagged partial/failed; the model decides whether to proceed with what it has                                |

Provenance records the abort/Finish with a synthetic reason in every case, so the graph stays complete even when the model saw only "the call failed."

---

## Streaming & auto-close

Streaming tools (`ToolCapability::Streaming`) need multiple reads, so **auto-finish must not fire after the first read**. The completion signal is the internal `session_read` result:

- While the internal read returns `Streaming`, the runtime keeps the session open and lets the model emit more `@N` reads.
- When the internal read returns `Done`, the runtime finishes.

The model emits reads as intent and never sees the open/close decision. The hazard to avoid in implementation: auto-finish keyed on `Send → Done` must be scoped to OneShot tools, **not** applied to Streaming sessions where the first `Done` may not be the stream's end.

---

## Provenance worked example (invariant proof)

Provenance completeness is an **invariant**: hiding ops from the model must not change the graph. Concretely, for slack-notify with the Tier 1 surface:

**What the LLM emits for the tool call (1 Send hop; the turn then ends with a separate `ReadOnlyFinish` reply hop — 2 model hops total):**

```json
{ "op": "Send", "input": { "text": "deploy ok", "context_id": "ctx-1" } }
 // SupportSlackNotifySendStep
```

**What the graph records (unchanged from the explicit 3-hop flow):**

```
Open    (session step; auto-open reason in the FSM op + tracing, not a graph field)
  → Send  (input = { text, context_id }; effect-token start)
    → SendDone            (session step; in graph, not surfaced as a model-facing line)
    → [optional SearchRead/PageRead if the model read @N]
  → (teardown: complete_session_lifecycle — token completion, not a Finish session-step node)
```

> Recorded session steps are `Open`/`SendDone`/`SearchRead`/`PageRead` only — there is no `Finish`/`Abort` node (true for explicit and runtime-driven teardown alike). See [H](#h-provenance-invariant).

**What the conversation projection shows the model:** a single Send result.

Two properties make this safe:

1. **Distinguishing runtime-driven from model-driven lifecycle in the graph is not yet possible** — lifecycle reasons (auto-open/auto-finish/auto-abort) live in tracing, not graph fields, and teardown has no node. Making this auditable needs reason-bearing `SessionStepOp` variants; deferred under [H](#h-provenance-invariant). It does not affect what the model sees.
2. **Graph-truth ≠ what the model sees is not new.** `SendDone` is already recorded in the graph and flows through projection, but is not surfaced as a model/user-facing history line (`conversation_history_snapshot.rs`, `view.rs`). Hiding Open/Finish from the model-facing view follows the exact same precedent — it does not invent a new divergence.

---

## Impact to achieve meaningful simplification

Rough implementation tiers. Estimates assume focused work across builder, step executor, tests, and fixture regen — not a precise schedule.

### Tier 1 — Low/medium impact, high value (recommended)

**~1–2 weeks focused work**

Extend runtime sugar; keep the existing typed BAML step types.

| Change                                                                            | Effect                                                                                                                                                                                                                                                                                               |
| --------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Entry-hop typed Send** when `open_input` is empty                               | Extend the entry union to emit `<Tool>SendStep` (incl. the multi-tool union), not just `<Tool>OpenStep`; reuses existing auto-open. **Builder change** (`session_from_ir/mod.rs:231-236`).                                                                                                           |
| **SendStep → tool resolution on entry**                                           | New **dispatch** path: resolve the tool from the step type (`FooSendStep → Foo → registry`), symmetric to `<Tool>OpenStep`. Today's resolution (`selected_tool` / `tool_name_for_scope` / `SessionPlan`) does not cover entry Sends.                                                                 |
| **Entry-FSM termination**                                                         | Entry hop must accept a Send that runs + auto-finishes, yielding `Finished`/`Done` and terminating — vs today's "entry must yield `status: open`" (`step_executor_loop.rs:657`). The `Finished → Terminal` transition already exists (`:653`); the work is producing that status from an entry Send. |
| **Auto-finish** after `Send → Done` for `Strict` + `OneShot` (excludes Streaming) | Eliminates the explicit Finish hop                                                                                                                                                                                                                                                                   |
| **Inferred auto-finish scope** (no new enum)                                      | Fires only for `OneShot + Strict` with no pending `@N` read; investigative/MultiSend excluded by capability/policy, not an opt-out flag                                                                                                                                                              |
| **Intent-level error contract**                                                   | Lifecycle failures surface as failed Send/read results, never as FSM errors                                                                                                                                                                                                                          |

**Touches:**

- `crates/baml-rt-builder/src/builder/baml_gen/session_from_ir/mod.rs` — entry union: add typed Sends alongside Opens (`:231-236`)
- `crates/baml-rt-quickjs/src/baml/tool_invocation_plan.rs` — resolve tool from `<Tool>SendStep` on entry
- `crates/baml-rt-quickjs/src/step_executor_loop.rs` — entry-FSM termination + auto-finish + error mapping (`:649-674`)
- Tests and fixture regen (`just regen-fixtures`)
- Agent prompts / coordination BAML where they duplicate FSM prose

**Does not require:** rewriting the external-tool protocol or the provenance event model.

**LLM experience:** slack-notify goes from **3 hops → 2** (`Send` + the trailing `ReadOnlyFinish` reply; the Open/Finish lifecycle ops are gone). A multi-tool observability pass goes from **~4 hops/query → 1 `Send`/query plus a single shared trailing `ReadOnlyFinish`** (so N queries = **N+1 hops**, not 4N), plus optional `@N` reads. The win is dropping the per-call lifecycle ops, not the model's final reply hop — true 1-hop would require chat-layer synthesis of the reply (deferred).

### Deferred: `Invoke` step (parked)

**Not on the critical path. Parked until measured catalog bloat justifies it.**

A unified, LLM-visible `Invoke { tool_name, input }` step would collapse Open+Send(+auto-finish) into one named shape at codegen time, replacing the per-tool `<Tool>OpenStep | <Tool>SendStep | <Tool>FinishStep` family with a single step plus a `tool_name` union.

Why it is **deferred, not adopted**:

- The **typed `<Tool>SendStep` union already covers multi-tool entry** (Tier 1) and does so via constrained decode, which is **more reliable for small models** than emitting a correct free-text `tool_name`.
- `Invoke` buys **catalog compression** (one step shape instead of N `*SendStep` types), not capability. That only pays off when the union of typed steps measurably bloats the schema catalog.
- Adopting it early would mean documenting two patterns ("use Send or Invoke depending on…") — strictly worse for prompt clarity.

Revisit only if (a) the typed-step union is shown to bloat the catalog or hurt decode, and (b) `tool_name`-string decode is shown to be reliable enough on the target models. Until then, Tier 1 typed-Send is the recommended end state, and `Invoke` is parked.

### Tier 3 — Full "tool calls only" semantics (rejected)

**Multi-month, architectural**

Replace the phase executor with function-call semantics everywhere; collapse or redesign the provenance model; rework external/MCP adapters, the archive read model, and MultiSend coordinators.

**Not recommended** unless willing to revise the runtime thesis and [conversation spec](assertions/baml-rt-conversation-spec.md).

---

## Migration / back-compat

Existing agents emit explicit `Open`/`Send`/`Finish` and must keep validating during the transition. Tier 1 is **additive**, not a hard break:

- **Entry union gains the typed Send** alongside the existing `<Tool>OpenStep`. Explicit `Open` on entry is still accepted — auto-open is an _addition_, not a replacement.
- **Auto-finish fires only for OneShot + Strict.** Agents that still emit an explicit `Finish` remain valid; `Finish` on an already-finished session is tolerated as a no-op.
- **Transient catalog bloat:** while both `<Tool>OpenStep` _and_ `<Tool>SendStep` sit in the entry union, the catalog grows by one type per eligible tool. Expected during migration. **Post-migration: drop `<Tool>OpenStep` from the entry union for eligible (empty-`open_input`) tools** so the union shrinks below today's size. Measure catalog size here before reconsidering [Invoke](#deferred-invoke-step-parked).
- **Active-phase end state:** for a pure one-shot the target is **entry `Send → Done` terminates the loop** — no `Active` hop at all. `Active__*` phases remain only for tools needing follow-on `Send`/`@N` reads. State this as the goal so implementers don't keep a vestigial Active hop for one-shots.
- **Regen fixtures** (`just regen-fixtures`) after the union/codegen change; hand-authored flows keep working.

This lets agents adopt the shorter surface incrementally rather than forcing a coordinated rewrite.

---

## Recommendation summary

| Question                                                                    | Answer                                                                                           |
| --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Is Send-only (runtime-managed Open/Finish) a regression?                    | **No**, if internal FSM + provenance stay                                                        |
| Are we removing Open/Finish from the internal protocol or provenance graph? | **No** — invariant; we only hide them from the LLM                                               |
| Would removing Open/Finish from the _internal_ protocol be a regression?    | **Yes**                                                                                          |
| How does the host know which tool on Send-only?                             | **Typed `<Tool>SendStep` + phase narrowing + manifest**; then auto-open                          |
| What about reads?                                                           | **Internal read** always host-owned; **`@N` archive reads** stay model-visible (they are intent) |
| What about multi-tool agents?                                               | **The default case** — typed Send union on entry; no `Invoke` needed                             |
| What about errors?                                                          | **Intent-level contract** — lifecycle failures never surface as FSM errors                       |
| Best path?                                                                  | **Tier 1 only** for now; `Invoke` parked; Tier 3 rejected                                        |
| Where current design still wins                                             | MCP/external sessions, large archive investigation, MultiSend accumulation, streaming            |

### Practical split by tool class (examples, not separate contracts)

| Tool class                      | Examples                                      | LLM surface                                                                                                                       |
| ------------------------------- | --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| **One-shot / write-only**       | slack-notify, email send, many internal tools | `Send` only; runtime owns Open/Finish                                                                                             |
| **Read-heavy / MCP / external** | Grafana MCP, claude/dev, sandbox tools        | `Send` + `@N` reads; lifecycle hidden, reads first-class                                                                          |
| **MultiSend coordinators**      | observability-coordinator                     | Independent sends → runtime-coalesced; accumulating sends → visible as intent (see [MultiSend](#multisend-the-one-open-decision)) |

---

## Gap today

The main gap is a **product / LLM-contract** issue, not a missing runtime capability.

The host already knows how to run one-shot tools end-to-end (`ToolRegistry::execute()`, strict auto-open on Send fragments, graph-only `SendDone`). The step executor still forces the model to narrate lifecycle steps the runtime could own — especially for tools like `support/slack_notify` where `OpenInput = ()` and the effect is a single post. Closing the gap is Tier 1: auto-finish, entry-hop typed Send, and an intent-level error contract.

---

## Related docs and code

| Topic                              | Reference                                                                             |
| ---------------------------------- | ------------------------------------------------------------------------------------- |
| Agent authoring / FSM              | [`docs/assertions/how-to-write-agents.md`](assertions/how-to-write-agents.md) §3      |
| Runtime thesis                     | [`docs/assertions/agentium-runtime-thesis.md`](assertions/agentium-runtime-thesis.md) |
| Host tools                         | [`docs/reference/host-tool-guide.md`](reference/host-tool-guide.md)                   |
| SessionPolicy / ToolCapability     | `crates/baml-rt-tools/src/tools.rs`                                                   |
| Step executor loop                 | `crates/baml-rt-quickjs/src/step_executor_loop.rs`                                    |
| Auto-open on Send                  | `crates/baml-rt-quickjs/src/baml/tool_session_plan.rs`                                |
| Tool invocation plan / ArchiveRead | `crates/baml-rt-quickjs/src/baml/tool_invocation_plan.rs`                             |
| Typed step codegen                 | `crates/baml-rt-builder` (`catalog_rendering_test.rs`, `return_shape.rs`)             |
| Graph-only events                  | `crates/baml-rt-conversation/src/projection.rs`, `conversation_history_snapshot.rs`   |
| slack-notify tool                  | `crates/tools/slack-notify/src/lib.rs`                                                |
| External session lifecycle         | `crates/baml-rt-tools/src/external_tools/sandbox/session_invoker.rs`                  |

---

## Tier 1 implementation checklist

Small, independently trackable sub-tasks. Ordered by critical path. Doc-side items already landed in this spec are checked.

### A. Metadata / eligibility (prerequisite for scoping)

- [x] Thread `ToolCapability` + `invocation_mode` into the exported builder catalog metadata (today exposes `session_policy` only)
- [x] Add eligibility helper: `entry_send_eligible(tool)` = `OneShot` + empty/optional `open_input` (reuse `schema_allows_empty_or_optional_open_input`)

### B. Builder — entry union (`session_from_ir/mod.rs:231-236`)

- [x] Emit `send_step(class_name)` for eligible tools into `entry_return`
- [x] `<Tool>SendStep` carries a literal `tool_name` field (symmetric to `<Tool>OpenStep`, `tool_interfaces.rs`) so polymorphic-entry Sends self-identify their tool for dispatch (see [C](#c-dispatch--resolve-tool-from-sendstep-tool_invocation_planrs)); the catalog picks it up via IR with no extra renderer change
- [x] Keep `open_step(...)` in the union (additive during migration)
- [x] Confirm archive reads / `ReadOnlyFinish` stay in the entry union
- [x] Update `catalog_rendering_test.rs` for the new entry Send types

### C. Dispatch — resolve tool from SendStep (`tool_invocation_plan.rs`)

- [x] Add resolution path: polymorphic-entry `<Tool>SendStep` → tool. **Mechanism note (revised):** BAML's serialized output drops the chosen union variant's class name (`serialize_partial` emits a `Class` as bare fields, no `__type`), so `FooSendStep → Foo` can't be read off the JSON. The first cut matched the Send `input` against candidate input schemas, but that is ambiguous when two tools share an input shape. **Adopted instead:** `<Tool>SendStep` now bakes a literal `tool_name` field, symmetric to `<Tool>OpenStep` (builder change in `tool_interfaces.rs`). Constrained decode auto-fills it (the model never types it), so `extract_tool_session_plan` lifts it into `selected_tool` and the existing `selected_tool.or_else(scope)` arm resolves it deterministically. **Dispatch therefore needed no new code** — the input-schema matcher was removed; the multi-candidate arm is back to its original `selected_tool.or_else(tool_name_for_scope)` shape.
- [x] Unit test: entry Send resolves the right tool without scope/open session (`extract_tool_session_plan_send_step_with_tool_name_pins_tool`; companion `…_without_tool_name_has_none` keeps extraction tolerant of the legacy tool_name-free shape)

### D. Step-executor FSM — entry termination (`step_executor_loop.rs:649-674`)

- [x] `Phase::Entry` accepts and runs a Send step — the existing `Entry + Done → continue` already handles it (no rejection); documented the invariant in the entry arm.
- [x] Entry Send → auto-finish (E) → `Done` → **loop continues (stateless resend)**, not force-terminate. **Semantics note:** one-shot and "independent sends" share one runtime rule (auto-finish per Send→Done); the model ends the turn with `ReadOnlyFinish` (existing `read_only_finish → Terminal`). Force-terminating after one Send would break multi-send agents, so it is *not* done. slack-notify is therefore Send→ReadOnlyFinish (2 hops); true 1-hop needs chat-layer synthesis (out of D/E scope).
- [x] Existing entry `Open` → bind → `Active` path unchanged (auto-finish is scoped to runtime-`auto_opened` sessions, so explicit `Open→Send→Finish` is byte-identical — `test_step_executor_loop_drives_full_session_with_interceptor` still asserts ≥3 hops).
- [x] Pure one-shot creates **no `Active` hop** (an entry Send never transitions to `Active`).

### E. Auto-finish (inferred, no new enum)

- [x] Auto-finish after `Send → Done` for `OneShot + Strict`, in `execute_tool_session_plan` (symmetric to auto-open). Scoped to `auto_opened` sessions only. "No pending `@N` read" holds automatically: Strict = one fragment per invocation, and `@N` reads hit the global ref table (session-independent), so closing never blocks a later read.
- [x] Exclude `Streaming` — gated on `capability == OneShot`, which inherently excludes `Streaming` (the only other variant).
- [~] Synthetic reason `auto-finish after Send→Done; OneShot+Strict` is emitted as a **tracing** log. Graph-node reason annotation is deferred: `tool_session_finish` takes no reason arg (unlike `tool_session_abort`); adding a reason-bearing finish is a small follow-up tracked under [H](#h-provenance-invariant).
- [x] No new `SessionPolicy` variant — rule derived from `capability` + `session_policy` metadata.
- [x] Integration test: `test_step_executor_loop_entry_send_auto_finishes_and_resends_statelessly` (two back-to-back entry Sends with no Open/Finish both reach Done, then ReadOnlyFinish terminates).

### F. Security / policy gate (Ryan)

- [x] Authz/policy check fires on the `Send` sub-step **before** the output read materializes. **Already true (verified):** the interceptor pipeline maps `InterceptorDecision::Block` → `Err` (`interceptor.rs:342`), and the send path runs `intercept_tool_call(&ctx).await?` (`tool_session_handle.rs:396`) *before* `session_send` (`:461`) — so a block propagates and the effect never commits. Same on the `execute()` path (`tool_execution.rs:223-238` returns before `tool_registry.execute()`). Holds inside the one-shot entry-Send host call: auto-open intercepts, then the Send intercepts, both before any output.
- [x] `Send` stays a distinct graph node — structurally guaranteed: a Send emits `SessionStepOp::SendDone`, reads emit `SearchRead`/`PageRead`; they are never the same node. (Teardown is token completion via `complete_session_lifecycle`, not a separate `Finish` node — see [H](#h-provenance-invariant).)
- [x] Test: `test_blocked_send_produces_no_effect_and_surfaces_block` — a `ToolInterceptor` allows the auto-open and blocks the calc **Send**; the loop fails with `blocked by interceptor`, the tool produces no Done output, and the gate is recorded as firing on the Send sub-step.

### G. Error contract (intent-level)

Scoped to the auto-opened entry-Send path (the new surface); the explicit `Open→Send→Finish` path is unchanged. All in `tool_session_plan.rs` via `send_unavailable_result` + the after-loop cleanup.

- [x] Auto-open failure → failed `Send` result. A synthetic open that errors returns `send_unavailable_result` (intent-level `status: "error"`, `LlmCorrectable`) instead of propagating a hard error; no session was created, nothing to clean up. Explicit model `Open` still propagates.
- [x] Mid-send failure → no dangling session. The after-loop cleanup now **aborts** an `auto_opened` session whose Send did not complete (the non-Fatal failure path already returns a failed-Send result via `plan_send_tool_error_value`; without the abort the session would linger and the next hop's reused session would hit a lifecycle error). Success still finishes (E). **Not changed:** Fatal tool failures and policy Blocks ([F](#f-security--policy-gate-ryan)) still hard-fail the turn (early return) — Fatal is unrecoverable and a security Block must not be made model-recoverable; their auto-opened-session cleanup on the early-return path is a small follow-up.
- [ ] Stream-read failure → partial/failed read flag — **deferred.** Streaming is a separate path (excluded from the one-shot auto-open/finish work); not part of this Tier-1 entry-Send slice.
- [x] No lifecycle-phrased error reaches the model surface. The bare-Send-to-a-config-tool rejection was reworded from `"strict auto-open is allowed only when…"` to an intent-level `send_unavailable_result`. Unit test `send_unavailable_result_is_intent_level_not_lifecycle` asserts the failed-Send result carries the tool + reason and contains none of `open before send` / `session already has input` / `finish before send` / `no open session` / `fsm`.

### H. Provenance invariant

**Correction after verifying the actual graph:** the recorded session steps are `Open`, `SendDone`, `SearchRead`, `PageRead` only (`SessionStepOp` in `baml-rt-core/src/bus.rs`). There is **no `Finish`/`Abort` session-step node** — session teardown runs through `complete_session_lifecycle` (closes the session, completes the effect token), for **both explicit and runtime-driven** finishes. So the entry-Send one-shot graph is `Open(auto) → Send/SendDone`, with teardown as token completion, not a distinct `Finish` node. The earlier "→ Finish(auto)" in the [worked example](#provenance-worked-example-invariant-proof) was aspirational; corrected there.

- [x] Graph for entry-Send one-shot recorded as `Open → SendDone` (+ effect-token start/complete); session teardown via `complete_session_lifecycle`. `Send`/`SendDone` is a distinct node from reads (see [F](#f-security--policy-gate-ryan)).
- [ ] **Deferred (cross-crate):** auto-open/auto-finish/auto-abort reasons are **tracing-only**, not graph fields — neither auto nor model-driven lifecycle reasons reach the graph today (`SessionStepOp::Open` carries no reason; there is no `Finish` node at all). Making runtime-driven vs model-driven distinguishable *in the graph* needs reason-bearing `SessionStepOp` variants (`Open { reason }`, new `Finish`/`Abort { reason }`) plus emit + projection + provenance-store handling — a deliberate change spanning `baml-rt-core`, `baml-rt-quickjs`, `baml-rt-conversation`, `baml-rt-provenance`. Not a safe single increment; tracked here.
- [x] Projection: the model sees a single Send result; lifecycle stays graph-internal. Locked by `test_entry_send_projection_hides_lifecycle_shows_single_send` (entry-Send one-shot → merged history has a tool line, no `SendDone`/`describe_open`/opcode/`Finish` leakage), reinforcing the existing `step_executor_intra_turn_*` snapshot guard which the entry-Send path inherits through the same projection.

### I. Tests + migration

- [x] `just regen-fixtures` — done during the B/C landing.
- [x] Existing explicit `Open`/`Send`/`Finish` agents still validate — auto-finish/abort is scoped to `auto_opened`, so explicit flows are byte-identical; `test_step_executor_loop_drives_full_session_with_interceptor` (Open→Send→Finish, ≥3 hops) still passes.
- [x] `Finish` on an already-finished session = no-op. In `execute_tool_session_plan` step-prep, a `Finish`/`Abort` fragment with no live session returns a closed status (`finished`/`aborted`) instead of the lifecycle-phrased `"session fragment rejected: no open session"` (a non-Open/non-read/non-Finish step with no session now returns an intent-level `send_unavailable_result`, not a hard error). Test: `test_explicit_finish_after_auto_finish_is_noop` (entry Send auto-finishes, then an explicit Finish resolves as a finished no-op and terminates the loop).
- [~] e2e: slack-notify **3 hops → 2** (`Send` + trailing `ReadOnlyFinish`) / multi-tool N typed Sends + 1 `ReadOnlyFinish` — **mechanism covered** by `test_step_executor_loop_entry_send_auto_finishes_and_resends_statelessly` (entry Send auto-opens/finishes; back-to-back Sends without Open/Finish) and the projection test, on the `support/calculate` (OneShot, empty-open) fixture. A *named-tool* slack-notify/multi-tool e2e would re-prove the same runtime path with heavier real-agent fixtures — not landed (low marginal confidence vs. setup cost).

### J. Post-migration (gates the parked Invoke decision)

- [ ] Measure entry catalog size with both `OpenStep` + `SendStep` present
- [ ] Drop `<Tool>OpenStep` from the entry union for eligible (empty-`open_input`) tools
- [ ] Re-measure; only reconsider `Invoke` if the typed-union catalog is still too large

### K. Docs

- [x] Reframe: intent vs lifecycle
- [x] Multi-tool default + typed Send union
- [x] Decision table
- [x] Error / recovery contract
- [x] Streaming guardrail
- [x] Provenance worked example
- [x] Policy-gate invariant (`Send` = block hook)
- [x] Migration / back-compat + catalog-bloat note
- [x] Update `how-to-write-agents.md §3` once entry-Send lands — §3 intro note + corrected the §3.4 entry-row ("No Send" was false) + new **§3.4a** "Entry-hop Send: runtime-owned Open/Finish for one-shot tools" (auto-open/finish, stateless resend, multi-tool, intent-level failures, MultiSend/Streaming exclusions, provenance/projection).

---

## Runtime hardening landed after entry-Send migration

After entry-hop typed `Send` shipped, a downstream SCADA/Grafana MCP demo exposed a failure mode: the model kept re-issuing identical investigative queries instead of using already-materialized `@N` archives. Root cause was runtime-side visibility: `SessionContext` only spotlighted the latest archive and the post-Send state still looked like `awaiting_open`.

Follow-up runtime changes landed in `0bc7894e fix: harden direct send step executor flow`.

### What changed

- **`SessionContext` contract bumped to `session_context_v3`**
  - Back-compat fields remain populated: `last_archive_ref`, `last_output_header`, etc.
  - New semantics are explicit instead of mutating `v2` silently.

- **Archive ledger added to `SessionContext`**
  - New field: `archives: [{ archive_ref, header }]`
  - Bounded to last 30 entries.
  - Headers are compact: use existing archive header identity, strip `@N` and size suffix (`· 7L · 2.8KB`), cap per-entry length.
  - Purpose: tell the model “you already have these tool results” without forcing `PageRead` or pasting full bodies.

- **Post-Send state made legible**
  - Entry-hop one-shot `Send → Done` now reports `status: "result_ready"` while `session_open: false`.
  - Meaning: previous direct Send completed; archive is available; read it or move on — do not infer “must open/send again.”
  - Entry-hop typed Send behavior remains unchanged: OneShot + Strict + empty-open still auto-opens, sends, reads to Done, auto-finishes, and stays stateless.

- **Identical Send dedup added**
  - Runtime records stable Send signature on archive entries: `tool_name + canonicalized input JSON`.
  - If the model emits the same `(tool, input)` again in the same context, runtime suppresses the tool call and returns a soft duplicate result pointing at the existing archive.
  - No duplicate archive is materialized.

Duplicate result shape:

```json
{
  "status": "duplicate",
  "op": "DuplicateSend",
  "archive_ref": "@1",
  "output": "@1 · mcp/grafana/query_prometheus:Send(expr=\"up\") · 7L · 2.8KB",
  "duplicate": true,
  "message": "Identical Send already materialized as @1; read it instead of re-issuing.",
  "result": null
}
```

### New generated prelude shape

```baml
class ArchiveLedgerEntry {
  archive_ref string @description("Archive handle, e.g. @7")
  header string @description("Compact tool/action identity for this Send, without result body")
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  status string @description("FSM status: awaiting_open | just_opened | result_ready | done. result_ready means previous direct Send completed and its archive is available; read it or move on, do not re-Send same input.")
  last_step_op string? @description("Previous FSM step op when known: open | send | duplicate_send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | duplicate | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
  archives ArchiveLedgerEntry[]? @description("Bounded ledger of Send archives already materialized this executor session. Use it to avoid identical Sends; PageRead/SearchRead existing refs when details are needed.")
}
```

Generated TypeScript now mirrors it:

```ts
export interface SessionContext {
  contract_version: "session_context_v3";
  session_open: boolean;
  status: "awaiting_open" | "just_opened" | "result_ready" | "done";
  last_step_op?: "open" | "send" | "duplicate_send" | "read" | "finish" | "abort";
  last_step_status?: "open" | "done" | "duplicate" | "finished" | "aborted";
  last_archive_ref?: string;
  last_output_header?: string;
  last_completion?: string;
  archives?: Array<{ archive_ref: string; header: string }>;
}
```

### Invariant after hardening

The simplified model contract remains: **model emits intent (`Send`, `PageRead`, `SearchRead`), runtime owns lifecycle**.

The hardening adds memory and guardrails around that contract:

1. Model sees what archives already exist.
2. Model sees that a direct Send result is ready, not that it needs to open/send again.
3. Runtime refuses to waste tool calls on identical Sends and points back to the existing `@N`.

This keeps reads optional and informed: full tool results still materialize inline on Send, and `PageRead` / `SearchRead` remain available when the model needs targeted lines from an archive.
