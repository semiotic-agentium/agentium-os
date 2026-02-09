# Test coverage and property-test candidates

This document summarizes current test coverage in `baml-rt-quickjs` and identifies what can be covered with **property testing** rather than example-based tests.

## Current coverage (example-based)

| Area | Tests | What they cover |
|------|-------|------------------|
| **Bridge** | `bridge_test.rs` | Creation, evaluate (no scope), evaluate JSON, concurrent scope propagation (8 tasks), stream scope propagation, tool session plan, execute/open without scope fails, invoke without scope fails, bindings export. |
| **BAML invoke/stream** | `baml_invoke_test.rs`, `baml_stream_test.rs` | One example each: invoke JS function, stream JS function. |
| **Sandbox** | `sandbox_test.rs` | require blocked, console.log works, fetch blocked. |
| **Effects** | `effect_property_test.rs` | Start/complete pairing, liveness gating, timeout monotonicity, in-flight counts, provenance admissibility; **one property test**: `prop_effect_pairing_random_sequence` (random start/complete sequences → in-flight count = started − completed). |
| **Execution** | `baml_execution.rs` (unit) | Export bindings, tool execution when variant present, non-tool results untouched. |
| **QuickJS bridge** | `quickjs_bridge.rs` (unit) | Concurrent tool invocations use tokens. |

Most tests are **single-example**: one input, one expected outcome. They don’t systematically vary inputs or assert invariants over large input spaces.

---

## Property-test candidates

These are areas where **invariants** hold for whole classes of inputs; encoding them as property tests gives better coverage and regression safety than more one-off examples.

### 1. **Tool extraction** (`baml/tool_extraction.rs`)

All of these need **in-crate** tests (`#[cfg(test)] mod tests` in the library) because the API is `pub(crate)`.

| Invariant | Semi-formal | Example today | Property test |
|-----------|-------------|---------------|---------------|
| **No tool_name in result** | ∀ result: if `extract_tool_call(result) = Ok(Some(call))` then call.args does not contain key `"tool_name"`. | Implicit in “tool identity from schema”. | `prop_extract_tool_call_never_returns_tool_name_key`: for any JSON object that passes extraction (with `__type` or single-key wrapper), the extracted `ToolCall.args` has no `tool_name` key. |
| **Round-trip for plan input** | ∀ value: if `normalize_plan_input(v) = Ok(w)` and v was a string, then parsing w back yields a Value. | None. | `prop_normalize_plan_input_string_roundtrip`: for any string s that is valid JSON, `normalize_plan_input(Value::String(s))` parses to the same structure. |
| **extract_tool_session_plan steps shape** | ∀ result with `steps` array: each step has `op`; no step has `tool_name`. | Validated by execution tests. | `prop_extract_plan_steps_have_op_no_tool_name`: for any JSON with `steps` array, extracted steps all have a valid op and none contain `tool_name`. |
| **resolve_tool_name uniqueness** | For registry R and input I: if exactly one tool’s schema matches I, then `resolve_tool_name_from_input_with_registry(R, I)` = that tool’s name; if 0 or ≥2 match, Err. | Single tool in tests. | `prop_resolve_tool_name_one_match_ok_multi_match_err`: with a small fixed registry (e.g. 2 tools with different required keys), generate inputs; assert Ok only when exactly one schema matches, Err otherwise. |

**Suggested location:** `src/baml/tool_extraction.rs` — add `#[cfg(test)] mod tests` with proptest (and possibly a small test registry) so `pub(crate)` functions are directly testable.

---

### 2. **Invocation tokens** (`quickjs_bridge/scope.rs`)

| Invariant | Semi-formal | Example today | Property test |
|-----------|-------------|---------------|---------------|
| **Token uniqueness** | ∀ distinct calls to `next_invocation_token()`: tokens are distinct. | Not tested. | `prop_invocation_tokens_unique`: generate N, collect tokens, assert set size = N. |
| **resolve_scope_from_token_arg** | ∀ map M, token t ∈ M: resolve with args `[t, ...]` returns Ok(scope); ∀ t ∉ M: Err. | Implicit in scope propagation tests. | `prop_resolve_scope_present_ok_absent_err`: build map, add/remove token, call resolve with token as first arg; assert Ok when present, Err when absent. |

**Suggested location:** In-crate again. Scope is `pub(crate)` and used via the bridge. Either a `#[cfg(test)] mod tests` in `quickjs_bridge/scope.rs` (if we can depend on proptest in dev and avoid pulling QuickJS), or a small unit test file under `src/` that only tests `next_invocation_token` and a mock map for `resolve_scope_from_token_arg` (no runtime).

---

### 3. **JS codegen** (`quickjs_bridge/js_codegen.rs`)

| Invariant | Semi-formal | Example today | Property test |
|-----------|-------------|---------------|---------------|
| **Prelude contains bindings** | ∀ scope, token_prelude: `build_scope_prelude(scope, token_prelude)` contains substring `__baml_invocation_token`, and context_id serialization. | None. | `prop_build_scope_prelude_contains_token_and_context`: for any `InvocationScope` and token string, output contains the token string and the serialized context_id. |
| **Wrapped promise code embeds token** | ∀ code_expr, token_literal: `build_wrapped_promise_code(code_expr, token_literal)` contains `token_literal` and `__set_eval_result`. | None. | `prop_build_wrapped_promise_contains_token_and_set_eval`: for any non-empty code and token strings, output contains both and `__set_eval_result`. |

**Suggested location:** `src/quickjs_bridge/js_codegen.rs` — `#[cfg(test)] mod tests`. Use proptest for string generation; scope can be built from proptest-generated context_id / message_id / task_id (or fixed values).

---

### 4. **Effect system** (already partially property-tested)

| Invariant | Semi-formal | Example today | Property test |
|-----------|-------------|---------------|---------------|
| **Start/complete pairing** | ∀ sequence of Started/Completed: in_flight = started − completed. | `prop_effect_pairing_random_sequence` (10 random ops). | Already present; can extend with more sequences or more effect kinds. |
| **Timeout monotonicity** | Re-checking timeout_attempts never decreases. | `test_timeout_monotonicity_effect_completion`. | `prop_timeout_attempts_monotonic`: over random effect sequences, each re-query is ≥ previous when no completion. |

Effect tests live in `tests/effect_property_test.rs` and use `baml_rt_core` types; no need for quickjs internals.

---

### 5. **Promise polling / eval result** (optional, heavier)

| Invariant | Semi-formal | Example today | Property test |
|-----------|-------------|---------------|---------------|
| **Timeout attempts only increase** | In poll loop, `timeout_attempts` is only ever updated with max(old, new). | Effect tests. | Could encode in a unit test for `EffectGatedPoller` with random in_flight sequences; already covered by effect tests. |

---

## Summary: implemented and remaining

### Implemented (in-crate property tests)

1. **tool_extraction** (`src/baml/tool_extraction.rs` `#[cfg(test)] mod tests`)  
   - `prop_extract_tool_call_args_never_contain_tool_name`: valid tool-call objects (with `__type`, no `tool_name`) → extracted args never contain `tool_name`.  
   - `prop_normalize_plan_input_string_roundtrip`: `normalize_plan_input(Value::String(json))` equals parsed value for valid JSON.  
   - `prop_extract_plan_steps_valid`: valid `steps` array (valid `op`, no `tool_name`) → extracted step count matches.

2. **js_codegen** (`src/quickjs_bridge/js_codegen.rs` `#[cfg(test)] mod tests`)  
   - `prop_build_scope_prelude_contains_token_and_context`: prelude contains `__baml_invocation_token` and `__baml_context_id`.  
   - `prop_build_wrapped_promise_contains_token_and_set_eval`: wrapped promise code contains token literal and `__set_eval_result`.

3. **scope** (`src/quickjs_bridge/scope.rs` `#[cfg(test)] mod tests`)  
   - `prop_invocation_tokens_unique`: N successive `next_invocation_token()` calls yield N distinct tokens.

### Remaining (optional)

- **resolve_tool_name_from_input_with_registry**: property test with a small fixed registry (1–2 tools, different required keys) to assert Ok only when exactly one schema matches.  
- **resolve_scope_from_token_arg**: would require constructing `JsValueFacade` (QuickJS runtime); keep as example-based or mock at a higher level.

### Keep as example-based

- Bridge creation, evaluate simple code, sandbox (require/fetch), and full flow tests (concurrent scope propagation, stream scope, tool session plan) are better as integration/example tests.

### Consolidation (tests/ into property-style or fewer tests)

- **bridge_test**: Merged `test_quickjs_evaluate_simple_code` and `test_quickjs_evaluate_json` into one `test_quickjs_evaluate_expressions` (loops over several expressions). Merged `test_execute_tool_without_scope_fails`, `test_open_tool_session_without_scope_fails`, `test_invoke_function_without_scope_fails` into one `test_operations_without_scope_fail`.
- **effect_property_test**: Extended `prop_effect_pairing_random_sequence` to `prop_effect_pairing_and_underflow` (ops 0=Start, 1=Complete, 2=Orphan Complete; asserts in_flight matches bus semantics). Removed redundant example tests: `test_effect_start_complete_pairing`, `test_effect_count_accuracy`, `test_effect_count_underflow_detection`, `test_effect_token_completion_clears_in_flight` (covered by the property test).
- **sandbox_test**: Merged `test_sandbox_prevents_require`, `test_sandbox_console_log_works`, `test_sandbox_prevents_fetch` into one `test_sandbox_environment` (one agent build, three checks).
- **baml_invoke_test** and **baml_stream_test**: Left as single integration tests (fixture + BAML; not suitable for property tests).

### Effect system

- Already has `prop_effect_pairing_random_sequence` in `tests/effect_property_test.rs`; optional extension: more effect kinds or longer sequences.

---

## Mechanics

- **Proptest**: Already a dev-dependency (`proptest = "1.5"`). Use for in-crate and integration property tests.
- **In-crate tests**: Add `#[cfg(test)] mod tests` in `tool_extraction.rs` and `js_codegen.rs` (and optionally `scope.rs`). For `scope.rs`, use only types that don’t pull in the full bridge (e.g. `HashMap<InvocationToken, RuntimeScope>` and a small `RuntimeScope` built by hand).
- **Arbitrary data**: For `extract_tool_call` / `extract_tool_session_plan`, use proptest’s `Value` strategy or a custom strategy that generates objects with/without `__type`, with/without `tool_name`, and `steps` arrays with valid `op` values.

This gives a clear split: **invariants over parsing, codegen, and tokens** → property tests in the library; **end-to-end and environment behavior** → keep as example-based tests.
