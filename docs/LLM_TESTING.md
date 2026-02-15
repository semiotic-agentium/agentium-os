# LLM Test Contract

This repo runs live LLM calls in CI. That is intentional. To keep CI reliable and failures meaningful,
LLM tests follow a shared contract.

## Categories

Category A: Deterministic runtime tests
- No live LLM calls
- Failures indicate a runtime bug

Category B: Live LLM contract tests
- Must hit the real provider
- Use standardized retry and timeout behavior
- Failures indicate either runtime bugs or provider instability

Category C: Load or soak tests
- Optional or nightly only
- Stress and latency characterization

## Standard Behavior For Live LLM Tests

- Acquire the shared LLM test gate (default concurrency 1)
- Retry with bounded per-attempt timeout and backoff
- Fail with a clear error if all attempts fail

Helper: `run_live_llm_with_retry` in `crates/test-support/src/common.rs`.

Configuration:
- `LLM_TEST_CONCURRENCY` controls the shared gate size
- Default is 1 to reduce provider throttling and flakiness

## Why This Matters

Live LLM tests validate end-to-end plumbing in real conditions. The shared contract ensures we do not
confuse provider flakiness with runtime regressions, and it prevents every test from reinventing
its own retry logic.
