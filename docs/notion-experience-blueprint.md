# Notion Experience Blueprint (Next Stage)

This blueprint turns the Notion demo into a clearer reflection of the system's
real purpose: **auditable, session-based agent execution with replayable evidence**.

## Current Delta: Notion vs ClickUp

What ClickUp already demonstrates well:

- Deterministic mock-backed end-to-end tests in `crates/baml-agent-runner/tests/runner_clickup_test.rs`
- Provenance assertions against `tool_call` and `tool_result`
- Mermaid API verification in test flow
- Mock-friendly tool base URL override (`CLICKUP_API_BASE_URL`)

What Notion was missing:

- No runner-level E2E parity tests
- No mock base URL override for deterministic tests
- Demo path lacked explicit operator affordances around trace replay and test parity

## Two Advisor Lenses

### 1) Don Norman (UX + Human-Centered Systems)

Guidance applied:

- Make system status visible at all times.
- Reduce cognitive load with good defaults and explicit affordances.
- Keep actions reversible and inspectable.

Concrete changes:

- Demo output now foregrounds captured `contextId`/`taskId` and replay commands.
- Notion docs now provide a staged CLI narrative for discovery, deterministic path, and trace replay.
- The flow keeps frontend concerns out of scope and focuses on runtime/provenance behavior.

### 2) Leslie Lamport (Invariants + Distributed Reasoning)

Guidance applied:

- Treat behavior as a set of invariants, not anecdotes.
- Verify end-to-end semantics in tests with deterministic fixtures.

Concrete changes:

- Notion tool now supports `NOTION_API_BASE_URL` for deterministic mocks.
- New Notion runner E2E tests validate:
  - tool invocation correctness
  - provenance capture
  - Mermaid export viability
- Base URL and typed-input routing invariants are explicitly tested.

## Invariants

### 1) Session Protocol Invariant

**Property:**

`∀ tool_session: Open -> Send -> Next -> (Finish XOR Abort)`

**Enforcement:** generated tool FSM schema + agent planner constraints.
**Testing:** runner E2E tests assert tool_call/tool_result events and terminal responses.

### 2) Read-Only Notion Invariant

**Property:**

`∀ notion_requests: action ∈ {SearchPages, GetPage, GetPageBlocks}`

No write endpoints are reachable from `support/notion`.

**Enforcement:** tool API surface in `crates/baml-rt-tools/src/notion.rs`.
**Testing:** mock E2E verifies only `/search`, `/pages/{id}`, `/blocks/{id}/children`.

### 3) Typed Input Routing Invariant

**Property:**

`NotionInput = NotionSearchPagesInput | NotionGetPageInput | NotionGetPageBlocksInput`

with `deny_unknown_fields` per variant.

**Enforcement:** untagged union in Rust type model.
**Testing:** Notion input deserialization tests in `crates/baml-rt-tools/src/notion.rs`.

### 4) Traceability Invariant

**Property:**

`∀ user_turn t: ∃ provenance_context(t) ∧ Mermaid(context_id(t)) != ∅`

**Enforcement:** GraphQLite provenance writer + `/contexts/{id}/mermaid` API.
**Testing:** runner E2E tests call Mermaid endpoint and assert sequence output.

## Demo Narrative (What Teammates Should See)

1. **Discovery Scene**
   Ask: "What are we working on right now?"
   Show: planning + read-only retrieval.

2. **Deterministic Scene**
   Ask with direct Notion ID.
   Show: direct-ID fast path (lower latency, fewer planner ambiguities).

3. **Evidence Scene**
   Export Mermaid trace from captured `contextId`.
   Show: same flow as protocol-level evidence, not just terminal output text.

This is the "agent system story" in one run: **intent -> plan -> tool -> response -> proof**.
