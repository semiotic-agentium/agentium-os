# Testing Handbook

Authoritative reference for how we test surfaces in this repo. The goal is to
exercise systems the way real users (and unreliable networks) do. Happy paths
are nice, adversarial slices keep us honest. This is a living document: update
it as code evolves and use the examples below as patterns to adapt, not fixed
recipes.

---

## Testing Philosophy

- **Prefer vertical slices over unit shards.** Exercise the public API of the
  system under test (BAML runtime, A2A handlers, provenance writer, CLI entrypoint)
  and let real dependencies run. The only code that may be "test only" is
  fixture scaffolding (e.g. test support helpers, testcontainers, fixtures).
- **Use the test-support crate for shared fixtures.** Common setup helpers live
  in `crates/test-support` and are reused across tests to keep setup consistent.
- **Tests are contracts, not documentation.** Assertions should verify behavior
  that matters: specific outputs, error conditions, state transitions, and
  invariants (e.g. provenance normalization, tool registration, protocol flow).
- **Adversaries welcome.** For every happy path add malformed inputs, retries,
  and timing edge cases. Production bugs arrive from edges, not averages.
- **Make async behavior explicit.** Prefer `#[tokio::test]` for async surfaces,
  and keep concurrency controlled and deterministic where possible.

---

## Test Layout and Fixtures

### Where Tests Live

- Crate-level tests: `crates/*/tests/*.rs`
- Shared fixtures: `tests/fixtures/` (agent fixtures under `tests/fixtures/agents/`)
- Test utilities: `crates/test-support`

### Fixture Helpers (test-support)

Use `crates/test-support` for setup and fixtures rather than duplicating logic:

- `setup_baml_runtime_default()` and `setup_baml_runtime_from_fixture()` for runtime setup
- `setup_bridge()` for QuickJS bridge setup
- `agent_fixture()` and `fixture_path()` for fixture files
- **`ensure_fixture_runtime_types()`** — call at the start of any E2E test that loads from
  `tests/fixtures/agents/` (e.g. `agent_fixture("stream-baml-tool")` or `setup_baml_runtime_from_fixture(...)`).
  Ensures fixture runtime types are regenerated once per test process before use.
- `require_api_key()` to gate tests that require `OPENROUTER_API_KEY`
- `ensure_baml_src_exists()` to skip tests when `baml_src` is missing

---

## Authoritative E2E and Test Layers

We keep **one authoritative E2E per behavior** to avoid overlapping coverage and flaky duplicates.

### E2E Authority (single source of truth)

- **Streaming E2E (full agent → stream):**
  `crates/baml-agent-runner/tests/runner_test.rs`
  - `test_e2e_stream_baml_tool`, `test_e2e_stream_js_tool` — full runner + fixture package + stream.
- **Tool/LLM E2E (single request):**
  `crates/baml-rt/tests/tool_calling_test.rs`
  - `test_e2e_voidship_baml_tool_calling` — single-request tool E2E from fixture.
- **Tool/LLM E2E (concurrent):**
  `crates/baml-rt/tests/tool_calling_test.rs`
  - `test_e2e_voidship_baml_tool_calling_concurrent` — concurrent tool calls with per-request scope;
  authoritative for concurrency correctness (per-conversation scope, no cross-contamination).

### Integration (non–full E2E)

- **A2A streaming protocol:** `crates/baml-rt-a2a/tests/task_streaming_test.rs` — protocol semantics,
  tool-call flows, subscribe; not full runner binary.
- **QuickJS scope propagation:** `crates/baml-rt-quickjs/tests/bridge_test.rs` — scope attribution
  under concurrency, tool session plans.

### Property tests (invariants)

- **Scope attribution:** `crates/baml-rt-a2a/tests/provenance_property_test.rs` — no cross-contamination
  of `context_id` across concurrent requests.
- **Tool session lifecycle:** `crates/baml-rt-quickjs/tests/tool_session_property_test.rs` — valid
  session plans (open → send* → next → finish) complete consistently.
- **Stream chunk order and finality:** `crates/baml-rt-a2a/tests/stream_property_test.rs` — yielded
  chunks preserve order; exactly one chunk is final.

### Concurrency coverage

- **E2E:** `test_e2e_voidship_baml_tool_calling_concurrent` (baml-rt) — multiple requests with
  distinct agent/context IDs; each request’s result must match its inputs (no cross-talk).
- **Integration:** `test_context_id_is_task_local_under_concurrency` (baml-rt-a2a provenance_context_test),
  `test_quickjs_concurrent_scope_propagation` (bridge_test) — scope and provenance attribution.
- **Property:** `prop_scope_attribution_no_cross_contamination` (provenance_property_test) — any
  N concurrent requests with distinct context_ids produce events only for those context_ids.

### Malformed and error-path E2E

All live in `crates/baml-rt-a2a/tests/a2a_malformed_and_error_paths_test.rs`. Purpose: verify invalid
JSON-RPC input and runtime error paths produce the expected error responses or stream content.

- **Malformed JSON-RPC:** `test_malformed_a2a_invalid_jsonrpc_version`, `test_malformed_a2a_unsupported_method`, `test_malformed_a2a_invalid_params` — wrong version, unsupported method, or invalid params yield a single error response.
- **Allowlist during streaming:** `test_allowlist_violation_during_stream` — JS opens a tool not in the runtime allowlist; stream must contain allowlist error.
- **Streaming tool failure:** `test_streaming_tool_failure_mid_stream` — tool returns `Err` mid-stream; stream must contain error content.
- **Concurrency mixed success/failure:** `test_concurrency_mixed_success_failure` — valid and malformed requests run concurrently; valid succeed, malformed return error.

### Test authority map (per-behavior)

Single source of truth for each behavior; do not add overlapping coverage elsewhere.

| Behavior | Authoritative location | Do not duplicate in |
|----------|-------------------------|---------------------|
| Schema/function discovery | `baml-rt/tests/invoke_test.rs` — `test_load_schema_discovers_functions` | `execution_test.rs` or other invoke tests |
| SimpleGreeting invocation (direct runtime) | `baml-rt/tests/invoke_test.rs` — `test_invoke_simple_greeting`; `execution_test.rs` — `test_load_and_execute_simple_greeting` (superset: list + invoke) | Additional tests that only assert “invoke SimpleGreeting” via same API |
| Tool result contract (no success wrapper, steps/result shape) | `baml-rt/tests/contracts_test.rs` — one direct BAML path, one JS-wrapper path | Extra contract tests with same setup and same assertions |
| Streaming chunk semantics (statusUpdate, artifactUpdate) | `baml-rt-a2a/tests/task_streaming_test.rs` — protocol-level | `runner_test.rs` E2E (assert wiring/operability only, not chunk shape) |
| Full streaming E2E (runner + package + stream) | `baml-agent-runner/tests/runner_test.rs` — `test_e2e_stream_baml_tool`, `test_e2e_stream_js_tool` | Additional full-runner streaming tests |
| Single-request tool E2E (ChooseCalcTool, execute) | `baml-rt/tests/tool_calling_test.rs` — `test_e2e_voidship_baml_tool_calling` | Other crates re-covering same vertical slice |
| Concurrent tool E2E (per-scope, no cross-talk) | `baml-rt/tests/tool_calling_test.rs` — `test_e2e_voidship_baml_tool_calling_concurrent` | Duplicate concurrency E2E |

### Do-not-duplicate checklist

Before adding a test that touches any of the following, check the authority map above and **do not** add duplicate coverage in the “Do not duplicate in” locations:

- **Function discovery** — authority: `invoke_test.rs::test_load_schema_discovers_functions`. Do not add the same assertion in `execution_test.rs` or elsewhere.
- **Streaming chunk semantics** (statusUpdate, artifactUpdate) — authority: `task_streaming_test.rs`. Runner E2E tests should assert wiring/operability only, not chunk shape.
- **Tool result contract** (no success wrapper, steps/result shape) — authority: `contracts_test.rs` (one direct BAML path, one JS-wrapper path). Do not add extra contract tests with the same setup and assertions.
- **SimpleGreeting entry-point coverage** — authority: `invoke_test.rs` and `execution_test.rs`. Do not add further tests that only assert “invoke SimpleGreeting” via the same API.

---

## Integration and E2E Tests

### Use the Real API Surface

- Call the same functions that production code uses. Do **not** reach into
  `#[cfg(test)]` helpers to bypass validation or internal state.
- Seed data through production APIs, not internals. If you need a helper, add it
  to `crates/test-support` and call production surfaces from there.
- Prefer `BamlRuntimeManager`, `QuickJSBridge`, `A2aRequestHandler`, and
  provenance writers as the main entry points.

### External Services

Some tests use real infrastructure:

- Provenance tests in `crates/baml-rt-provenance/tests/` use in-memory SurrealDB
  or file-backed stores; no external graph server required.

---

## Snapshot Testing

We use `insta` for JSON snapshots in provenance tests:

- Snapshots live in `crates/baml-rt-provenance/tests/snapshots/`
- Example usage: `insta::assert_json_snapshot!` in
  `crates/baml-rt-provenance/tests/surreal_snapshot_test.rs`
- Update snapshots with `cargo insta review` after intentional changes

Keep snapshot inputs deterministic where possible (fixed IDs, stable ordering,
normalized data) to avoid noisy churn.

---

## Adversarial Testing

- For every new feature, brainstorm how it fails: duplicate requests, malformed
  inputs, replayed messages, stale graph state, unexpected tool payloads.
- When writing an “expected” success test, ask: _What happens if we swap this
  ID, if two clients race, if the runtime returns a partial result?_ Add those
  cases.
- Favor explicit assertions that explain _why_ the behavior is required.

---

## Invariants and Behavior Contracts

Invariants should be encoded as direct assertions in tests, and where appropriate as
**property tests** (e.g. `proptest`), especially in:

- Provenance normalization and relation derivation
  (`crates/baml-rt-provenance/tests/normalizer_test.rs`)
- Provenance persistence and graph shape
  (`crates/baml-rt-provenance/tests/store_test.rs`)
- Tool registration and execution correctness
  (`crates/baml-rt/tests/tool_calling_test.rs`)
- **Scope attribution:** property test that no provenance event has a `context_id` from a
  different request (`provenance_property_test::prop_scope_attribution_no_cross_contamination`).
- **Tool session lifecycle:** property test that valid session plans complete
  (`tool_session_property_test::prop_valid_session_plans_complete`).
- **Stream order and finality:** property test that K chunks yield K responses in order and
  exactly one final (`stream_property_test::prop_stream_chunk_order_and_finality`).

### How to Discover New Invariants

- **Start from the contract.** Walk the public API surface and ask: _what must
  be true before and after every call?_
- **Trace data flow across boundaries.** Follow a request through layers
  (runtime → bridge → tool execution → provenance).
- **Interrogate failure modes.** Review TODOs and past failures; turn them into
  assertions.
- **Consider conservation and exclusivity.** Look for quantities or relationships
  that must never be violated (single tool registration, unique IDs, graph edge
  consistency).

### Capturing the Invariants

1. **Name them explicitly.** Document each invariant inside the test module.
2. **Encode with helpers.** Wrap complex checks in reusable assertion functions.
3. **Add negative examples.** Create failing fixtures where useful.
4. **Keep invariants living.** Update them when behavior evolves.

---

## Concurrency and Async Testing

Most async tests use `#[tokio::test]`. When adding concurrency:

- Keep the test deterministic by controlling inputs and expected outcomes.
- Prefer explicit synchronization (`join!`, `try_join!`, and task joins).
- Ensure shared resources are not leaked between tests (fixtures are scoped per test).

---

## Example Playbook

1. **Integration test a new runtime feature.**
   - Use `setup_baml_runtime_default()` or `setup_baml_runtime_from_fixture()`.
   - Drive the public runtime APIs; no internal shortcuts.
   - Add both happy path and adversarial inputs.

2. **Provenance change.**
   - Update normalization tests and store tests.
   - Add or adjust `insta` snapshots if the graph shape changes.

3. **New tool or bridge behavior.**
   - Register the tool via `BamlRuntimeManager`.
   - Verify execution and JS bridge registration via `QuickJSBridge`.

---

## Quick Checklist Before Shipping

- Does the test call only production APIs (fixtures aside)?
- Are failure cases covered, not just success?
- Are snapshots deterministic and reviewed after changes?
- Are async/concurrency behaviors explicitly asserted?
- Are shared fixtures and external services properly scoped and cleaned up?

Following these practices keeps the test suite reliable as the codebase evolves.
