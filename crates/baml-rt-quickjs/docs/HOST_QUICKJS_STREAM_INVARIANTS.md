# Host/QuickJS stream interface — invariant analysis

This document records invariant and liveness properties of the host/QuickJS boundary for A2A stream requests. The protocol is encoded in type-safe components in `a2a_stream` with semi-formal annotations.

## Type-safe components

- **`A2aYieldSession<'a, S>`** — Linear session for one stream request; `S` ∈ {`YieldBufferReady`, `InvocationComplete`}.
- **`begin_a2a_yield_session(bridge)`** → `Result<A2aYieldSession<'_, YieldBufferReady>>` — Setup; caller must eventually call `invoke`.
- **`A2aYieldSession::invoke(self, request)`** (on `YieldBufferReady`) → `Result<A2aYieldSession<'_, InvocationComplete>>` — Run JS; caller must eventually call `collect`.
- **`A2aYieldSession::collect(self)`** (on `InvocationComplete`) → `Result<Vec<Value>>` — Read buffer; returns in finite time.

The type system enforces the ordering: `collect` cannot be called before `invoke`, and `invoke` cannot be called before `begin`.

## Liveness (semi-formal)

Notation: □ = always, ◇ = eventually.

| ID | Property | Meaning |
|----|----------|--------|
| **L1** | □(begin returns Ok(s) → ◇(invoke(s) called)) | After setup succeeds, caller eventually invokes. |
| **L2** | □(invoke returns Ok(s′) → ◇(collect(s′) called)) | After invoke succeeds, caller eventually collects. |
| **L3** | □(collect called → ◇(collect returns)) | Collect terminates in finite time. |
| **L4** | Promise poll loop: after at most max_attempts_ms steps, loop exits (__eval_result set or error) | Bounded exit; does not distinguish "waiting on I/O" from "will never yield". |

## Data flow

1. **Host** holds `bridge: Arc<Mutex<QuickJSBridge>>` and runs, under one lock, the session:  
   `begin_a2a_yield_session(&mut *bridge)` → `session.invoke(js_request)` → `session.collect()`.
2. **JS** sees `globalThis.__baml_a2a_yield_buffer` and `globalThis.__baml_a2a_yield` (set by setup).  
   `handle_a2a_request` runs; it may call `__baml_a2a_yield(chunk)` and may `await` (e.g. `openToolSession`, BAML). The stream handler typically **does not terminate** (no final promise resolution).
3. **Host** reads chunks from the yield buffer via `get_a2a_yield_buffer()` at a time of its choosing (e.g. after a timeout or when the buffer has grown); it does **not** wait for the JS promise to resolve. The return value of `handle_a2a_request` is ignored.

## Invariants

### 1. Single-writer yield buffer (per stream request)

**Property:**

- Before each stream request, the host sets `globalThis.__baml_a2a_yield_buffer = []` and `globalThis.__baml_a2a_yield`; only the current `handle_a2a_request` call may push to that buffer for the duration of that call.
- After the promise for that call resolves, the host reads and clears the buffer exactly once.

**Formal:**

- ∀ stream request r:  
  `setup_a2a_yield_buffer()` runs once before `invoke_js_function(..., r)`;  
  `get_a2a_yield_buffer()` runs when the host decides to read (e.g. after a timeout or poll); the JS promise for that invoke typically **does not resolve** (stream is non-terminating).  
  No other code clears or replaces `__baml_a2a_yield_buffer` between setup and get.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | Invoker holds bridge lock for full sequence; no other caller runs JS.     |
| Contract     | a2a.ts: agent must call `__baml_a2a_yield(chunk)`; host ignores return.    |
| Testing     | `stream_request_uses_only_invoke_stream_chunks` (mock invoker).            |

---

### 2. No bridge re-entrancy while stream invoke is in progress

**Property:**

- While the host is inside `invoke_stream` (bridge lock held) and waiting for the `handle_a2a_request` promise to resolve, JS must not call back into host code that requires the **same** bridge lock (e.g. another `invoke_js_function` or `evaluate` that would block on that lock).

**Formal:**

- ∀ execution where Host holds `lock(bridge)` and is in `evaluate()` polling loop waiting for promise P:  
  any callback or future that contributes to resolving P must not perform a blocking acquisition of `lock(bridge)`.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | Tool sessions (e.g. BAML tools) use `baml_manager` and registry, not bridge. A2A tool sessions that call `handle_a2a` would re-enter; avoid opening A2A sessions from inside a stream `handle_a2a_request`. |
| Concurrency | Bridge is `Mutex`; re-entrant lock would deadlock.                        |
| Testing     | Stream tests that hang may be hitting this (JS awaiting a path that needs the bridge). |

**Risk:** If JS does `await openToolSession("...")` and that tool’s `send`/`next` eventually calls the same agent’s `handle_a2a`, the handler will try to take the bridge lock → deadlock.

---

### 3. Non-stream evals: promise resolution observable after running pending jobs

**Property:**

- When `evaluate()` runs **non-stream** code that returns a promise, the host waits by: (a) running the runtime’s pending jobs (JS microtasks / promise continuations), (b) then checking `globalThis.__eval_result`. The runtime must allow promise continuations to run so that the async IIFE that sets `__eval_result` can complete. **Stream** evals do not wait for the promise to resolve; the host reads the yield buffer on its own schedule.

**Formal:**

- ∀ **non-stream** promise P returned from the eval’d code:  
  eventually, after some finite sequence of `run_pending_jobs_if_any()` and checks, `__eval_result` is set (or we hit max_attempts_ms and error).
- So: running pending jobs must be sufficient (or we must have another way) for the microtask that sets `__eval_result` to run.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | Polling loop in `evaluate()`: run `exe_rt_task_in_event_loop(rt.run_pending_jobs_if_any())`, then yield/sleep, then check `__eval_result`. |
| Fix         | Run pending jobs **before** the first check in each iteration so the event loop can process completions before we read. |

**Observed:** If we check first and only then run pending jobs, the first iteration never sees `__eval_result`; the continuation that sets it runs in `run_pending_jobs_if_any()`. Running jobs **before** the check each time makes resolution observable as soon as the runtime has processed the queue.

---

### 4. Yield buffer read uses same JSON path as write

**Property:**

- Chunks written in JS via `__baml_a2a_yield(chunk)` are read by the host as `Value::Array` after `JSON.stringify(buf)` in JS and `evaluate()` (which parses the returned string as JSON). The shape must match what the stream normalizer and pipeline expect.

**Formal:**

- `get_a2a_yield_buffer()` evals code that does `buf = __baml_a2a_yield_buffer; __baml_a2a_yield_buffer = []; return JSON.stringify(buf);`.  
  Host parses the eval result as JSON; if it is an array, that array is the chunk list; otherwise host uses `[]`.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | `get_a2a_yield_buffer` uses `evaluate()` so the string is parsed once in Rust. |
| Contract    | Agent yields plain objects (message/task/statusUpdate/artifactUpdate); normalizer accepts them. |

---

## Summary table

| ID | Invariant                          | Violation symptom        | Mitigation                                      |
|----|------------------------------------|---------------------------|-------------------------------------------------|
| 1  | Single-writer buffer per request   | Wrong or missing chunks  | One lock for setup→invoke→get; no fallback.    |
| 2  | No bridge re-entrancy during invoke | Hang (deadlock)         | No A2A session from inside stream handler.      |
| 3  | Promise resolution observable     | Hang (infinite poll loop) | Run pending jobs before each __eval_result check. |
| 4  | JSON round-trip for buffer         | Bad chunks / parse error | evaluate() for get_a2a_yield_buffer.           |

## Change applied

- **Invariant 3:** In `evaluate()`’s promise-polling loop, call `exe_rt_task_in_event_loop(rt.run_pending_jobs_if_any())` **before** evaluating the check script that reads `globalThis.__eval_result`, so the runtime can run promise continuations before we look. This preserves the invariant that “after running pending jobs, __eval_result is visible if the promise has resolved.”

---

## Liveness: waiting on I/O vs. will never yield

We need to distinguish two cases so the host can treat them differently:

| Environment | Description | Host should |
|-------------|-------------|-------------|
| **Waiting on tool/LLM** | JS is suspended at an `await` whose resolution depends on external work (tool call, LLM, network). Progress is possible once that work completes. | Keep polling (or allow long timeout); do not treat as "stuck" prematurely. |
| **Will never yield** | JS is in an infinite synchronous loop, or blocked on a lock that cannot be released (e.g. re-entrancy deadlock), or the runtime never runs the continuation that sets `__eval_result`. No amount of waiting will resolve the promise. | Detect and fail (e.g. bounded attempts, or explicit "no progress" signal). |

### Semi-formal distinction

- **Progress predicate** (for one poll iteration):  
  `progress(i) ≜ (__eval_result became set) ∨ (yield buffer grew) ∨ (some Rust future backing a JS promise made progress)`.

- **Waiting on I/O**:  
  □(¬progress(i) for many i) ∧ (there exists an external dependency D such that when D completes, progress will hold).  
  So: no progress *yet*, but progress is *possible*.

- **Will never yield**:  
  □(¬progress(i) for all i) ∧ (no external dependency can change that).  
  So: no progress and none possible (e.g. deadlock or infinite sync work).

### Implications for the current design

1. **Configurable MAX_ATTEMPTS (default: 1.8M × 1ms ≈ 30 minutes)**
   - Set via `QuickJSConfig::with_max_attempts_ms(Some(ms))`
   - Default: 1,800,000ms (30 minutes)  
   The poll loop cannot tell "waiting on LLM" from "deadlock". Both hit the same cap and return an error. So:
   - **Liveness L4** holds in a bounded sense: we eventually exit the loop (either with result or with error), but we do *not* guarantee "if the promise can resolve, we see it" — we might time out first when JS is legitimately slow (e.g. LLM).

2. **Making progress observable**  
   To separate "slow but progressing" from "stuck", we would need at least one of:
   - **Progress hint from JS**: e.g. the agent or runtime sets a "waiting on" flag (tool name, LLM call id) so the host can allow a longer or unbounded wait for that dependency.
   - **Progress hint from Rust**: when a Rust-backed promise (e.g. tool session, BAML call) is polled and makes progress, signal it so the host knows not to give up.
   - **Heuristic**: if the yield buffer has grown since last check, we are "making progress" (agent is yielding chunks while doing async work); if it never grows and we never see __eval_result, we are more likely in "will never yield."

3. **Refined liveness wording**  
   - **L4 (current, bounded):** After at most max_attempts_ms iterations, the loop exits (with __eval_result set or with error). So: "no infinite poll without *exit*."
   - **L4′ (strong, if we had progress detection):** If the JS promise will eventually resolve (no deadlock, no infinite sync), then ◇(__eval_result is set). We do *not* currently guarantee L4′ because we time out regardless of whether the promise could have resolved later.
   - **L4″ (will never yield):** If the environment will never yield (deadlock or infinite sync), then ◇(loop exits with error). We guarantee this only in the sense that we eventually hit max_attempts_ms and return an error; we do not *detect* "will never yield" explicitly.

### Recommendations

- **Document** the distinction (waiting on I/O vs. will never yield) and that the current loop is bounded (L4), not strong (L4′).
- **Optional**: Add a configurable timeout (or per-request override) so that callers who know an invocation is slow (e.g. LLM) can allow more attempts.
- **Optional**: In the future, if the runtime or agent can expose "waiting on X," the host can use that to avoid treating slow-but-progressing work as "stuck."

---

## Effects-first liveness gating (L5-L6)

### Overview

The effects-first system distinguishes "waiting on effect" (tool/LLM execution in progress) from "will never yield" (deadlock/infinite sync) by tracking in-flight effects per context. When effects are active, the host uses a longer timeout; when no effects are active, a shorter idle timeout applies.

### Architecture

**Effects drive provenance**: Tool/LLM executors emit `EffectEvent` (Started/Completed) via `EffectEmitter`. The `EffectBus` maintains in-flight counts per `ContextId` and fans out to subscribers (e.g. `ProvenanceEffectSubscriber` that converts effects to provenance events).

**Liveness gating**: `QuickJSBridge::evaluate()` checks `EffectLiveness::in_flight(context_id)` in the poll loop:
- If `in_flight(context_id).any() > 0`: use `max_attempts_ms` (configurable, default 30 minutes) — effects are active, progress is possible.
- If `in_flight(context_id).any() == 0`: use `idle_timeout_ms` (default 5s, configurable) — no effects, likely stuck.

### Semi-formal properties

| ID | Property | Meaning |
|----|----------|---------|
| **L5** | Effect-gated timeout: if `in_flight(ctx_id) > 0`, use long timeout; else use short timeout | Distinguishes "waiting on effect" from "will never yield". |
| **L6** | Completion events always fire: `Started` → `Completed` (success or error) | Prevents "forever in-flight" states. |

**Formal (L5):**

- ∀ poll iteration i in `evaluate()`:  
  `timeout_attempts(i) = if in_flight(context_id).any() then MAX_ATTEMPTS else idle_timeout_ms`
- So: when effects are active, we allow longer waits; when idle, we fail fast.

**Formal (L6):**

- ∀ effect emission: if `EffectEvent::ToolStarted { context_id, .. }` or `EffectEvent::LlmStarted { context_id, .. }` is emitted, then eventually `EffectEvent::ToolCompleted { context_id, .. }` or `EffectEvent::LlmCompleted { context_id, .. }` is emitted (even on error).

**Enforcement:**

| Layer | Mechanism |
|------|-----------|
| Execution | Tool/LLM paths emit `Started` before effect, `Completed` in all completion/error paths. |
| Liveness | `QuickJSBridge` queries `EffectLiveness` in poll loop; applies timeout based on `in_flight()`. |
| Scope | Context-only scope: `in_flight(context_id)` counts all effects for that context (concurrent requests in same context suppress timeouts). |

### Implementation

1. **Effect types** (`baml_rt_core::effects`):
   - `EffectKind::{Tool, Llm}`
   - `EffectEvent::{ToolStarted, ToolCompleted, LlmStarted, LlmCompleted}` with metadata
   - `EffectEmitter` trait (async `emit(EffectEvent)`)
   - `EffectLiveness` trait (async `in_flight(context_id) -> InFlightCounts`)

2. **EffectBus** (`baml_rt_core::effects::EffectBus`):
   - Maintains `HashMap<ContextId, InFlightCounts>` (increments on Started, decrements on Completed)
   - Subscribers receive events (e.g. `ProvenanceEffectSubscriber` converts to `ProvEvent`)

3. **Wiring** (`RuntimeBuilder`):
   - If `provenance_writer` is provided, create `EffectBus`
   - Subscribe `ProvenanceEffectSubscriber` to bus
   - Set bus as `EffectEmitter` on `BamlRuntimeManager` and `BamlExecutor`
   - Set bus as `EffectLiveness` on `QuickJSBridge`

4. **Execution sites**:
   - `BamlRuntimeManager::execute_tool()`: emit `ToolStarted` before execution, `ToolCompleted` after (all paths)
   - `baml_pre_execution::intercept_llm_call_pre_execution()`: emit `LlmStarted` before LLM call
   - `BamlLLMCollector::process_trace_events()`: emit `LlmCompleted` after LLM call

5. **Liveness gating** (`QuickJSBridge::evaluate()` poll loop):
   - Query `effect_liveness.in_flight(context_id)` each iteration
   - Apply `idle_timeout_ms` if no effects, `max_attempts_ms` if effects active

### Configuration

- `QuickJSConfig::idle_timeout_ms`: Short timeout when no effects (default: 5000ms)
- `QuickJSConfig::max_attempts_ms`: Long timeout when effects are in-flight (default: 1,800,000ms = 30 minutes)
- Set via `RuntimeBuilder::with_quickjs_config(QuickJSConfig::new().with_idle_timeout_ms(Some(ms)).with_max_attempts_ms(Some(ms)))`

### Limitations

- **Context-only scope**: Concurrent requests in the same context will suppress timeouts (acceptable per design).
- **No per-effect tracking**: We track counts, not individual effect IDs (sufficient for liveness gating).
- **Completion guarantee**: Relies on executor correctness (all paths emit Completed); no automatic cleanup on panic.
