# Citable history & checked citations (vs PUD-style evidence)

This note captures **why** planning and provenance use **ref-table `citations`** (`Vec<String>` in the
`#N` / `@N` vocabulary) instead of opaque “evidence” strings or parallel “derived message id” fields.

**See also:** [Intent-based planning & session prompting](intent-based-planning-and-session-prompting.md) — its core principles state that intents and step transitions carry **citations** (ref-table `#N` / `@N`), not evidence prose, so the stack keeps **citable history** and **checked citations** in provenance/drift.

## What is novel relative to typical PUD-style flows

| Dimension | PUD / archive-centric baseline | This runtime |
|-----------|----------------------------------|--------------|
| **What can be cited** | Often **tool/archive blobs only** (e.g. `@N` or attachment-like evidence) | **Session history and archives**: `#N` indexes into `conversation_history` / ref table; `@N` / `@N:L` for archives — **citable history**, not archive-only. |
| **What gets stored on intent / step** | Short **evidence prose** or untyped ids | **The same citation strings** the model (or shim) used, so provenance can **reconcile** with the ref table and scoring. |
| **Downstream treatment** | Treat evidence as display text | **Parse → check**: `ParsedCitation` in `baml_rt_tools::citations`, drift / BIPIA signals over **resolved** content, negation (`!#N`), etc. |

So the **novel bit** is the **unified citation contract** across: prompt ref table → planning submit →
`EffectEvent` → `ProvEventData` → normalizer graph attributes — with **validation and embedding checks**
on those citations, not just carrying human-readable “reason” strings.

## Wire and Rust surfaces (stable names)

- **JS / execution session**: `intent.citations`, `start_step` / `complete_step` payload `citations` (arrays).
- **Core bus**: `EffectEvent::IntentResolved { citations, .. }`, `PlanStepStatusChanged { citations, .. }`.
- **Provenance**: `ProvEventData::IntentResolved { citations, .. }`, `PlanStepStatusChanged { citations, .. }`;
  normalizer persists them as JSON arrays on entities where applicable.

## References in code

- Ref grammar and parsing: `crates/baml-rt-tools/src/citations.rs` (`ParsedCitation`, negation, line ranges).
- Drift / citation scoring: `baml-rt-provenance` effect subscriber + `baml-rt-embedding` (e.g. BIPIA / drift catalogue).
- Planning FSM: `crates/baml-rt-quickjs/src/planning.rs`, `execution_session_types.rs`, `quickjs_bridge/baml_registration.rs`.

## Related docs

- **Intent/plan prompting:** `docs/intent-based-planning-and-session-prompting.md` (same contract in operator-facing guidance: citations → citable history + checked provenance).
- Drift / injection framing: `docs/drift-catalogue.md`
