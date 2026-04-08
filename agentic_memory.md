# Agentic Memory — Provenance + Targeted Read Strategy

## Main Objective

Use provenance archive refs (`@N`) as the primary memory handle, and keep prompts lean by default.

Do **not** inline large tool payloads into `conversation_history` across hops.

Instead:

1. Keep archive headers/citations visible (e.g., `@18 support/clickup ...`).
2. Use `Read` only when needed.
3. Prefer **targeted Read** (`grep` + pagination) based on the user’s explicit entity (task name/id, etc.).
4. Return only the narrow evidence needed for the current turn.

---

## Why this is the right default

Large `tasks:` blocks in prompt history consume tokens and increase latency.

The citation/archive mechanism already supports efficient retrieval:

- `Send` produces a stable archive ref (`@N`)
- Follow-up turns can `Read` that ref with a pattern (`grep`) and bounds (`offset`, `limit`)
- This avoids replaying entire prior payloads

This should be generic for all tools, not business-specific.

---

## Example (target behavior)

1. User: "which tasks are in progress?"
2. Tool returns list archive: `@18`
3. Agent may `Read @18` to gather required evidence and answer.
4. User: "What is the priority of Task19?"
5. Agent should **not** replay full list. It should issue targeted read:
   - `Read { archive_ref: "@18", grep: "Task19", ... }`
6. Agent answers from those narrow lines.

---

## Current status from latest logs (`clickup2.logs`)

Observed positives:

- Duplicate projection suppression is active:
  - `@18 read view already shown (offset=0, limit=200)` appears repeatedly.
- Exhausted-view guard is active:
  - `read view already exhausted (archive_ref=@18, grep=None, offset=200, limit=10)` appears in output.

Observed remaining pain:

- Prompt payloads still spike in follow-up paths:
  - up to ~`14459`–`14518` bytes in `ChooseClickUpAction__continue__support_clickup`.
  - ~`13092` in `ChooseClickUpAction__select` in later turn.
- So we reduced replay noise, but still carry too much context in some hops.

---

## What can be improved next (generic)

## A) Agent implementation level

These are prompt/planning behavior improvements inside agents (tool-agnostic patterns):

1. **Entity-first retrieval rule (short, explicit)**
   - If user provides a concrete entity token (name/id), try targeted `Read` with `grep` before broad reads.
   - Keep instruction concise; avoid long catalogs of special cases.

2. **Evidence sufficiency discipline**
   - For detail questions (single task/item): stop after first sufficient targeted slice.
   - For aggregate/all-items questions: continue paging until completion criteria are met.

3. **Compact final answer policy**
   - Prefer concise answer with citations instead of echoing large retrieved content.
   - Keep tool evidence in provenance, not in user-facing body.

4. **Continuation prompt minimization**
   - Ensure continue-phase prompt asks for one of: `Send` / `Read` / `Finish` with strict JSON only.
   - Avoid repeating large prose instructions each hop.

## B) Host/runtime level

These are system-level improvements benefiting all agents:

1. **SendDone header-first projection mode**
   - Default to header-only for large archives (`@N ... [lines, size]`), body on-demand via explicit `Read`.
   - Inline full body only for small archives.

2. **Function-aware compact context profiles**
   - Keep compact projection for executor phases (`__select`, `__act__`, `__continue__`).
   - Tune profile with fixed structural budgets (rows/bytes), not business logic.

3. **History budget enforcement by bytes**
   - Apply deterministic max-bytes cap after projection.
   - Evict oldest low-priority entries first, preserving latest user intent + latest relevant `@N` evidence.

4. **Read query observability**
   - Track metrics for targeted vs broad reads:
     - `% Reads with grep`
     - `% follow-up turns using existing @N`
     - avg `prompt_payload_bytes` by phase
   - Use this to tune defaults safely.

5. **Archive discoverability affordance (small prompt hint)**
   - Keep one short invariant hint in executor prompts:
     - "Use existing `@N` refs first; for entity-specific asks, prefer `Read` with grep."

---

## Guardrails (avoid brittle logic)

- No hardcoded domain IDs or tool-specific business branches.
- No fixed "small limit = X" assumption globally.
- No giant instructional prompt trees.
- Keep policies structural, bounded, and generic.

---

## Success criteria

1. Prompt payload trend decreases for follow-up hops (especially continue/select).
2. More entity-targeted reads (`grep` usage rises on detail queries).
3. Fewer broad replays of list-like archives in later turns.
4. Equal or better answer quality with explicit citations.
5. Improvements transfer across tools/agents (not only ClickUp).

---

## Execution roadmap (checklist)

## P0 — Highest ROI (do first)

- [ ] **Host: SendDone header-first for large archives**
  - Keep `@N` header visible; inline body only for small archives.
  - Require explicit `Read` for large body drilldown.

- [ ] **Host: deterministic conversation-history byte budget**
  - Enforce max bytes after projection.
  - Preserve newest user intent + latest relevant `@N` evidence first.

- [ ] **Agent: tiny invariant retrieval rule**
  - "Reuse existing `@N` refs first; for entity-specific asks, prefer `Read` with `grep`."
  - Keep this short and stable (no large prompt catalogs).

## P1 — Next optimization wave

- [ ] **Host: tighten compact profile for executor hops**
  - Tune row/byte budgets for `__select`, `__act__`, `__continue__`.
  - Validate no loss of required evidence for step completion.

- [ ] **Agent: continuation prompt minimization**
  - Keep continue-phase prompt operational and JSON-only (`Send`/`Read`/`Finish`).
  - Reduce repeated prose across hops.

- [ ] **Host: add retrieval observability metrics**
  - `% Reads with grep`
  - `% follow-up turns reusing existing @N`
  - prompt bytes by phase (`select/act/continue`)

## P2 — Quality and regression hardening

- [ ] **Agent: compact final answer policy**
  - concise answer + citations, avoid reprinting large evidence.

- [ ] **Host: projection regression tests**
  - Large archive should not re-inline by default.
  - Byte-budget clipping preserves critical latest evidence.

---

## Suggested sequence

- [ ] **Sprint A:** P0 items
- [ ] **Sprint B:** P1 profile + prompt tightening
- [ ] **Sprint C:** P1 observability + P2 hardening