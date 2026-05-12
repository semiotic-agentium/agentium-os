# Tool-session FSM: current runtime/generator analysis and improvement plan

## Executive summary

The Rust step-executor FSM is internally typestate-clean, but the generated BAML phase functions expose a narrower and more rigid interaction surface than agent authors expect. In the current design, every session-plan turn starts in a SELECT phase that can only choose/open a tool (plus any non-session return types from the parent function). Archive reads and `Finish` are only available after a tool has already been opened. That makes reuse of already-visible archive data structurally awkward: the model must open a session before it can read archived output, even if the turn should be answerable from existing context.

The main issue is not one specific tool. It is the contract created by the runtime FSM + generated phase prompts + generated narrow unions:

- **SELECT** decides whether to open a tool session. It cannot inspect archives through `SearchRead` / `PageRead` and cannot finish a read-only reuse path.
- **ACT** is the first post-open phase. It can `Send`, `SearchRead`, `PageRead`, or `Abort`, but cannot `Finish`.
- **CONTINUE** is the later post-done phase. It can do the same as ACT plus `Finish`.

This creates extra hops, more generated BAML surface, duplicated prompts, and confusing constraints for agent authors. The most promising improvement is to redesign the generated session surface around **ENTRY** and **ACTIVE** phases, or at minimum to widen SELECT so it can read/finish without opening a tool session.

---

## Current Rust FSM

Defined in `crates/baml-rt-quickjs/src/step_executor_loop.rs`.

Current phase representation:

```rust
enum Phase {
    AwaitingOpen,
    Bound {
        tool: ToolBinding,
        status: OpenStatus,
    },
    Terminal(TerminalReason),
}

enum OpenStatus {
    JustOpened,
    Done,
}
```

Current per-phase function selection:

```rust
fn phase_function_id(base: &BamlPromptName, phase: &Phase) -> BamlFunctionId {
    match phase {
        Phase::AwaitingOpen => BamlFunctionId::variant(base.clone(), VariantPhase::Select),
        Phase::Bound { tool, status: OpenStatus::JustOpened } => {
            BamlFunctionId::variant(base.clone(), VariantPhase::Act { tool_slug: tool.slug.to_string() })
        }
        Phase::Bound { tool, status: OpenStatus::Done } => {
            BamlFunctionId::variant(base.clone(), VariantPhase::Continue { tool_slug: tool.slug.to_string() })
        }
        Phase::Terminal(_) => BamlFunctionId::base(base.as_str()),
    }
}
```

Current transition behavior:

| Current phase | Step status | Runtime behavior |
|---|---:|---|
| `AwaitingOpen` | `open` | bind selected tool, move to `Bound { JustOpened }` |
| `AwaitingOpen` | `done` | loop back to SELECT without binding a tool |
| `AwaitingOpen` | missing status | terminal `MissingStatus` |
| `AwaitingOpen` | anything else, e.g. `finished` / `aborted` | contract violation error |
| `Bound { JustOpened }` | `done` / legacy done-like statuses | move to `Bound { Done }` |
| `Bound { JustOpened }` | `aborted` | terminal `Aborted` |
| `Bound { JustOpened }` | `finished` | terminal `Finished`, but the generated ACT union does not expose `Finish` |
| `Bound { Done }` | `done` / legacy done-like statuses | stay in `Bound { Done }` |
| `Bound { Done }` | `finished` | terminal `Finished` |
| `Bound { Done }` | `aborted` | terminal `Aborted` |

The FSM itself is valid: impossible states are mostly unrepresentable, tool binding is immutable after `Open`, and CONTINUE cannot happen before ACT. The ergonomic problems come from which operations the generated phase functions allow.

---

## Current generated BAML phase functions

Defined in `crates/baml-rt-builder/src/builder/baml_gen/session_from_ir/mod.rs`.

For every parent BAML function that returns one or more `*SessionPlan` types, the builder emits:

```text
<Base>__select
<Base>__act__<tool_slug>
<Base>__continue__<tool_slug>
```

For one base function with `N` candidate tools, this is:

```text
1 SELECT + N ACT + N CONTINUE = 2N + 1 generated phase functions
```

### SELECT return union

Current generation:

```rust
let non_plan_types: Vec<String> = collect_union_type_names(&func_sig.output)
    .into_iter()
    .filter(|t| !t.ends_with("SessionPlan"))
    .collect();

let open_types: Vec<String> = candidates.iter()
    .map(|t| SessionTypeNames::open_step(&t.class_name))
    .collect();

let mut select_return = non_plan_types.clone();
select_return.extend(open_types);
```

So SELECT can return:

- per-tool `*OpenStep`
- non-session return types from the parent function, if any, such as `Report`, `AskUser`, `string`, etc.

SELECT cannot return:

- `*SearchReadStep`
- `*PageReadStep`
- `*FinishStep`
- `*AbortStep`, unless an abort-like type is part of the parent non-plan union

Generated SELECT prompt prefix/suffix currently reinforce open-first behavior:

```text
[OPEN] Open a session with: <tool_list>.

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step ...
```

Important note: `crates/baml-rt-builder/src/builder/baml_gen/session_from_ir/phase_prompt.rs` contains a more flexible cue that says SELECT can read visible archives, but that helper is currently not wired into `mod.rs`. The generated BAML path in use is the local prompt builder in `mod.rs`, whose SELECT text says open-only.

### ACT return union

Current generation:

```rust
function <Base>__act__<tool>(...) ->
  <Tool>SendStep |
  <Tool>SearchReadStep |
  <Tool>PageReadStep |
  <Tool>AbortStep
```

ACT means: a tool session has just been opened, but no done-producing operation has yet completed. It allows `Send`, archive reads, and `Abort`. It does not allow `Finish`.

### CONTINUE return union

Current generation:

```rust
function <Base>__continue__<tool>(...) ->
  <Tool>SendStep |
  <Tool>SearchReadStep |
  <Tool>PageReadStep |
  <Tool>FinishStep |
  <Tool>AbortStep
```

CONTINUE means: at least one done-producing operation has happened. It allows the same operations as ACT plus `Finish`.

This means ACT and CONTINUE are almost identical. Their only generated-schema difference is whether `Finish` is legal.

---

## Current generated prompt behavior

The generated phase prompts are not only schemas; they actively steer the model.

Current generated SELECT prompt starts with:

```text
[OPEN] Open a session with: <tools>.
```

Current generated ACT prompt starts with roughly:

```text
[ACT] A <tool> session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.
```

Current generated CONTINUE prompt starts with roughly:

```text
[CONTINUE] <tool> result is archived.
Archive fallback when content must be inspected:
- Use the visible @N archive handle from conversation/history; do not guess one.
- See "@N <tool>" followed by numbered lines → content is inline; decide from that content and tool-specific instructions.
- See "@N <tool>" with "more lines" indicator → emit SearchRead or PageRead to paginate.
- See "@N <tool>" with no content yet → emit SearchRead or PageRead against the visible @N.
- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern.
- Do not re-Send the same work solely because archive body content is compact; inspect the archive when needed.
```

This has two consequences:

1. SELECT is framed as an open-only phase, so archive reuse is unavailable before a session exists.
2. CONTINUE often encourages pagination when a partial archive window says more lines exist. That is correct for some listing-style tools, but too aggressive for compact outputs where the visible lines already answer the user.

Agent-authored prompts are then embedded inside these generated prompts. If an agent author describes a fixed trajectory such as `Open → Send → PageRead → Finish`, that further cements the multi-hop path. However, the root issue exists independently of any one agent prompt: the generated SELECT union still cannot read or finish.

---

## Current TypeScript surface

Generated `.d.ts` files expose the full session-plan type, for example:

```ts
export interface SomeToolSessionPlan {
  step:
    | SomeToolOpenStep
    | SomeToolSendStep
    | SomeToolSearchReadStep
    | SomeToolPageReadStep
    | SomeToolFinishStep
    | SomeToolAbortStep;
  citations: string[];
}
```

They do not expose the per-phase narrowed unions as public types. Agent authors can see the broad plan shape, but not the actual phase legality:

- SELECT: open-only plus non-plan types
- ACT: send/read/abort
- CONTINUE: send/read/finish/abort

Current `RunContext` is also minimal:

```ts
export interface RunContext {
  text: string;
  message: ChatMessage;
  emit: SessionEmitter;
}
```

There is no typed archive index or helper such as `ctx.archives()`. Agent authors who want to deduplicate or reuse prior tool outputs must rely on prompt-visible history rather than a stable typed API.

---

## Structural issues

### 1. Archive reuse is impossible at entry

If the answer can be produced from a visible archive `@N`, the ideal action is either:

- finish immediately with citations to already-visible/read lines, or
- perform one archive read, then finish.

Current SELECT cannot emit archive reads or finish. Therefore the model must first open a tool session before it can inspect archive content through the FSM. This creates unnecessary session state, provenance, transcript noise, and at least one additional LLM hop.

### 2. There is no clean read-only terminal path

A reuse-only flow has no first-class terminal operation from SELECT. If SELECT emits a done-like result, Rust loops back to SELECT. If it emits `Finished`, Rust currently treats that as a contract violation in `AwaitingOpen`. The only normal terminal path is through a bound tool session.

### 3. ACT and CONTINUE duplicate most behavior

ACT and CONTINUE differ almost only by `Finish`. This creates:

- extra generated functions
- extra generated prompt text
- extra snapshots/fixtures
- more concepts for agent authors to understand
- more naming and trace surface to maintain

The underlying runtime bit is simple: `has_done = false` immediately after open, then `has_done = true` after a done-producing operation. The generated surface currently expands that one bit into two nearly identical phases.

### 4. Generated pagination guidance is globally applied

The CONTINUE prompt tells the model to page/search when an archive has a “more lines” indicator. That is useful for large search/listing tools, but not universally correct. For compact outputs, visible lines may already contain all fields needed for the final answer. A global rule can make the model spend hops reading data that is irrelevant or already visible.

### 5. Repeated archive reads self-loop

In `Bound { Done }`, `SearchRead` or `PageRead` returns `done`, which keeps the phase in CONTINUE. If the model emits the same archive read repeatedly, the runtime has no current cycle breaker; `max_steps` is the only backstop.

### 6. Agent authors do not have a typed phase contract

The broad `*SessionPlan` union tells authors every operation exists, but not when each operation is legal. The actual legality is hidden in generated BAML phase functions. This makes prompt writing harder and encourages authors to encode phase mechanics in natural language, which can drift from the generator/runtime contract.

---

## Design goal

Make the runtime-provided session contract simpler and more directly useful:

- Entry phase should support reuse-or-open decisions.
- Read-only archive reuse should not require opening a tool session.
- The generated prompt should express policy-neutral mechanics, not force one generic pagination strategy for every tool.
- Agent prompts should focus on domain policy: freshness, when to refresh, how to answer, and what evidence is required.
- TypeScript should expose enough typed context for agent-side short-circuiting and deduplication.

---

## Proposed target design: ENTRY / ACTIVE

Replace SELECT / ACT / CONTINUE with two conceptual phases.

| New phase | Runtime meaning | Legal operations |
|---|---|---|
| `ENTRY` | no tool is bound yet; decide reuse, ask/report, or open fresh session | per-tool `Open`, archive `SearchRead`, archive `PageRead`, read-only `Finish`, non-plan returns such as `AskUser` / `Report`, possibly `Abort` |
| `ACTIVE` | a specific tool is bound | tool `Send`, archive `SearchRead`, archive `PageRead`, tool `Finish`, tool `Abort` |

Runtime state can be simplified to:

```rust
enum Phase {
    Entry,
    Active {
        tool: ToolBinding,
        has_done: bool,
    },
    Terminal(TerminalReason),
}
```

Function selection becomes:

```rust
Entry -> <Base>__entry
Active { tool, .. } -> <Base>__active__<tool_slug>
```

For one base function with `N` candidate tools, generation becomes:

```text
1 ENTRY + N ACTIVE = N + 1 generated phase functions
```

### ENTRY behavior

ENTRY should allow three paths:

1. **Reuse existing archive**
   - `SearchRead` / `PageRead` an existing `@N`, then `Finish`, or directly `Finish` if cited visible content is already sufficient by policy.
2. **Open fresh session**
   - emit `Open`, bind the tool, move to ACTIVE with `has_done = false`.
3. **Non-tool response**
   - ask/report/return a non-plan type if the parent function allows it.

`Finished` from ENTRY should be a legal terminal transition.

### ACTIVE behavior

ACTIVE should allow:

- `Send`
- `SearchRead`
- `PageRead`
- `Finish`
- `Abort`

The runtime can enforce that `Finish` requires `has_done == true`, or the generated prompt can strongly instruct that. Runtime enforcement is safer.

Any `done` status flips `has_done` to true. `aborted` and `finished` terminal statuses exit the loop.

---

## Incremental path with lower migration risk

A full rename from SELECT/ACT/CONTINUE to ENTRY/ACTIVE is clean but touches many files. A safer sequence:

### Step 1: Fix generated prompt accuracy

- Remove stale/unwired `phase_prompt.rs`, or wire it properly.
- Make the actual generated SELECT prompt match its actual union. Today open-only text is accurate; if SELECT is widened, update the text in the same change.
- Reduce generic “more lines → paginate” pressure. Prefer: “If visible lines are sufficient, finish; otherwise read more.”

### Step 2: Add cycle detection for repeated archive reads

In `step_executor_loop.rs`, track the previous archive-read signature:

```text
(op, archive_ref, offset, limit, grep)
```

If the same read repeats consecutively in a bound done phase, stop the loop or inject feedback. Options:

- terminal with warning
- return a synthetic done result saying content was already read
- force a final synthesis path if available

This is valuable even before the larger FSM redesign.

### Step 3: Surface archive freshness in headers

`crates/baml-rt-tools/src/archive_refs.rs::ArchiveEntry::display_header` currently emits approximately:

```text
@N · tool:Action(args) · 70L · 1.7KB
```

Add freshness if available:

```text
@N · tool:Action(args) · 70L · 1.7KB · 3m ago
```

This helps the model and agent policy decide whether reuse is acceptable.

### Step 4: Add typed archive access to `RunContext`

Expose a stable helper:

```ts
export interface ArchiveSummary {
  ref: string;
  tool: string;
  identity: string;
  fetched_at?: string;
  line_count?: number;
  byte_count?: number;
}

export interface RunContext {
  text: string;
  message: ChatMessage;
  emit: SessionEmitter;
  archives(): ArchiveSummary[];
}
```

This lets agent code deduplicate before invoking the BAML step executor.

### Step 5: Widen SELECT as a compatibility bridge

Before deleting ACT/CONTINUE, widen SELECT to include archive read and read-only finish operations:

```text
<Base>__select ->
  non-plan types |
  *OpenStep |
  ArchiveSearchReadStep |
  ArchivePageReadStep |
  ReadOnlyFinishStep
```

Then update `AwaitingOpen` transitions:

- `open` → bind tool
- `done` from archive read → remain in SELECT or move to a read-only done state
- `finished` → terminal finished
- `aborted` → terminal aborted, if supported

This gives immediate archive-reuse capability without removing ACT/CONTINUE yet.

### Step 6: Introduce ENTRY/ACTIVE behind a feature flag

Generate `__entry` and `__active__<tool>` alongside existing functions. Feature-flag the runtime dispatch to choose either:

- legacy: `__select`, `__act__`, `__continue__`
- new: `__entry`, `__active__`

After fixtures and agents are migrated, remove the legacy phase family.

---

## Builder/runtime checklist

### Runtime/core

- [ ] Add `VariantPhase::Entry` and `VariantPhase::Active { tool_slug }` in `crates/baml-rt-core/src/function_id.rs`.
- [ ] Add matching names in `crates/baml-rt-tools/src/tools.rs::SessionTypeNames`.
- [ ] Simplify `step_executor_loop.rs` phase state to `Entry` / `Active { has_done }`.
- [ ] Allow `Finished` from entry/read-only flows.
- [ ] Gate `Finish` in ACTIVE on `has_done`.
- [ ] Add repeated archive-read cycle detection.

### Builder

- [ ] Generate `__entry` with open + archive-read + read-only-finish + non-plan return types.
- [ ] Generate one `__active__<tool>` per tool.
- [ ] Drop or deprecate `__act__` / `__continue__` generation after migration.
- [ ] Align generated prompt prefixes/suffixes exactly with each phase union.
- [ ] Make pagination guidance tool-policy-aware or less prescriptive.

### TypeScript/codegen

- [ ] Export phase-narrowed step types.
- [ ] Add `RunContext.archives()`.
- [ ] Optionally expose helper utilities for matching archive identity/freshness.

### Archive/projection

- [ ] Add freshness to archive display headers.
- [ ] Consider marking repeated reads within a turn as already-read in the projected context.

### Tests

- [ ] Snapshot generated BAML for one-tool and multi-tool session-plan functions.
- [ ] Test entry archive reuse without opening a tool session.
- [ ] Test fresh path still works: entry open → active send → active finish.
- [ ] Test repeated archive-read cycle breaker.
- [ ] Test TS declarations include phase-narrowed types and `RunContext.archives()`.

---

## Recommended near-term fixes before full redesign

If we want practical improvement quickly without committing to the full ENTRY/ACTIVE migration immediately:

1. **Fix generated CONTINUE pagination language**
   - Replace “more lines → emit SearchRead or PageRead” with “if visible lines are insufficient, emit SearchRead or PageRead; otherwise Finish.”

2. **Add repeated-read detection**
   - Prevents max-step burn and makes failures easier to debug.

3. **Expose archive freshness in headers**
   - Low-risk and helpful for reuse decisions.

4. **Expose `ctx.archives()`**
   - Lets agent authors implement deterministic reuse policies outside LLM prompting.

5. **Prototype widened SELECT**
   - The smallest FSM-level change that unlocks read-only reuse.

---

## Bottom line

The current runtime FSM is not broken, but the generated session contract is heavier than it needs to be. It forces an open-first workflow, duplicates ACT/CONTINUE surfaces for a one-bit distinction, and hides phase legality from TypeScript agent authors.

The strategic fix is to make the generated contract match the real decisions an agent needs to make:

```text
ENTRY: reuse existing archive, ask/report, or open fresh work.
ACTIVE: operate within the selected tool session until finished/aborted.
```

That would reduce hops, reduce generated prompt size, make archive reuse first-class, and make the agent authoring model easier to understand.
