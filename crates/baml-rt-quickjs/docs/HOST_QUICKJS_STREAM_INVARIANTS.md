# Host/QuickJS stream interface — invariant analysis

This document records invariant and liveness properties of the host/QuickJS boundary for A2A stream requests. The protocol is encoded in type-safe components in `a2a_stream` with semi-formal annotations.

## Type-safe components

- **`A2aYieldSessionReady<'a>`** — Session in ready phase (yield buffer installed); only `invoke` may be called.
- **`A2aYieldSessionComplete<'a>`** — Session after `invoke`; holds `session_id` and `yield_rx` (host-only; never exposed to JS). Only `collect` may be called; invalid state (collect without invoke) is unrepresentable.
- **`begin_a2a_yield_session(bridge)`** → `Result<A2aYieldSessionReady<'_>>` — Setup (no-op for channels; per-session channel created on invoke); caller must eventually call `invoke`.
- **`A2aYieldSessionReady::invoke(self, scope, request)`** → `Result<A2aYieldSessionComplete<'_>>` — Run JS via `invoke_js_function_stream`; returns session with `session_id` and `yield_rx`; caller must eventually call `collect`.
- **`A2aYieldSessionComplete::collect(self)`** → `Result<StreamResult>` — Drains session’s `yield_rx` via `drain_yield_buffer`; calls `finalize_a2a_stream_invocation(session_id)` on completion. Returns in finite time.

The type system enforces the ordering: `collect` exists only on `A2aYieldSessionComplete`; `invoke` exists only on `A2aYieldSessionReady`.

## Liveness (semi-formal)

Notation: □ = always, ◇ = eventually.

| ID | Property | Meaning |
|----|----------|--------|
| **L1** | □(begin returns Ok(s) → ◇(invoke(s) called)) | After setup succeeds, caller eventually invokes. |
| **L2** | □(invoke returns Ok(s′) → ◇(collect(s′) called)) | After invoke succeeds, caller eventually collects. |
| **L3** | □(collect called → ◇(collect returns)) | Collect terminates in finite time. |
| **L4** | Promise poll loop: after at most max_attempts_ms steps, loop exits (__eval_result set or error) | Bounded exit; does not distinguish "waiting on I/O" from "will never yield". |
| **L6** | Stream promise non-termination: the promise from `onChatMessage()` is designed never to resolve; chunks are yielded via `__chat_yield` and collected by the host. | Host never waits on stream promise resolution. |

## Data flow

1. **Host** runs the session: `begin_a2a_yield_session(&mut bridge)` → `ready.invoke(scope, js_request)` → `complete.collect()`. Each `invoke_js_function_stream` acquires the stream semaphore (one permit), creates a per-session channel, sets the host-only `current_stream_session_id_slot`, and runs the stream IIFE.
2. **JS** sees `globalThis.__chat_yield` set by the **stream IIFE** to `function(chunk) { __baml_chat_yield_host(JSON.stringify(chunk)); }`. No host state (e.g. session id) is passed into JS. Scope for tool/baml is resolved by the host via LIFO (context entered when the stream starts).
3. **Host** routes yields: `__baml_chat_yield_host(chunk_json)` reads `current_stream_session_id_slot`, looks up the sender in `a2a_yield_tx_by_session`, and sends the parsed chunk. The caller drains its session’s receiver via `drain_yield_buffer(rx)` and finalizes with `finalize_a2a_stream_invocation(session_id)`.

## Invariants

### 1. Single-active-stream (one permit)

**Property:**

- At most one stream invocation holds the stream semaphore permit at a time. The bridge uses `Semaphore::new(1)`. So at most one “active” stream is running JS for yield purposes; concurrent stream requests are serialized at invoke.

**Formal:**

- ∀ time t: |{ s : stream session s holds the permit at t }| ≤ 1.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | `invoke_js_function_stream` acquires `stream_semaphore.clone().acquire_owned()`; the permit is stored in `StreamInvocationSession` and released when the session is removed in `finalize_a2a_stream_invocation(session_id)`. |
| Concurrency | Only one permit; no cross-stream concurrency of JS execution for streams.  |

---

### 2. Host-only session identity (no host state in JS)

**Property:**

- `StreamSessionId` and `current_stream_session_id_slot` are never exposed to JS. JS never receives or sends a session id. Routing of yields is done entirely in the host.

**Formal:**

- ∀ chunk yielded from JS: the host function `__baml_chat_yield_host(chunk_json)` receives only the chunk payload; session identity is read from `current_stream_session_id_slot` in the host. No JS variable or argument carries `StreamSessionId`.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | IIFE sets only `globalThis.__chat_yield = function(chunk) { __baml_chat_yield_host(JSON.stringify(chunk)); }`; no `__sid` or `__streamSessionId` in args or globals. |
| Host        | `register_chat_yield_host` registers `__baml_chat_yield_host(chunk_json)`; implementation reads `current_stream_session_id_slot` and `a2a_yield_tx_by_session`. |
| Testing     | No session id in any JS fixture or shim for stream yield.                  |

---

### 3. Per-session channel and current-stream slot

**Property:**

- Each stream has its own channel `(tx, rx)`. The sender is stored in `a2a_yield_tx_by_session[session_id]`; the receiver is returned to the caller from `invoke_js_function_stream` and drained via `drain_yield_buffer(rx)`. When a stream starts, `current_stream_session_id_slot` is set to that session’s id; when it is finalized, the slot is cleared (if it matches). Yields from JS go to the channel of the current stream only.

**Formal:**

- ∀ stream session s with id `sid`: at start, `current_stream_session_id_slot = Some(sid)` and `a2a_yield_tx_by_session[sid] = tx`; at finalize, slot is cleared for `sid`, session and `tx` are removed. ∀ call to `__baml_chat_yield_host`: chunk is sent to `a2a_yield_tx_by_session[current_stream_session_id_slot]` when the slot is `Some`.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | `start_stream_session` creates channel, inserts tx, sets slot; `finalize_a2a_stream_invocation(session_id)` clears slot and removes session and tx. `__baml_chat_yield_host` reads slot and map. |
| Contract    | Caller uses the `rx` returned from `invoke_js_function_stream` for `drain_yield_buffer` and calls `finalize_a2a_stream_invocation(session_id)` when done. |

---

### 4. No bridge re-entrancy while stream invoke is in progress

**Property:**

- While the host is inside `invoke_stream` (bridge lock held) and waiting for the `onChatMessage` promise to resolve, JS must not call back into host code that requires the **same** bridge lock (e.g. another `invoke_js_function` or `evaluate` that would block on that lock).

**Formal:**

- ∀ execution where Host holds `lock(bridge)` and is in `evaluate()` polling loop waiting for promise P:
  any callback or future that contributes to resolving P must not perform a blocking acquisition of `lock(bridge)`.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | Tool sessions (e.g. BAML tools) use `baml_manager` and registry, not bridge. A2A tool sessions that call `handle_a2a` would re-enter; avoid opening A2A sessions from inside a stream `onChatMessage`. |
| Concurrency | Bridge is `Mutex`; re-entrant lock would deadlock.                        |
| Testing     | Stream tests that hang may be hitting this (JS awaiting a path that needs the bridge). |

**Risk:** If JS does `await openToolSession("...")` and that tool’s `send`/`next` eventually calls the same agent’s `handle_a2a`, the handler will try to take the bridge lock → deadlock.

**Concurrent A2A and tool execution:** We need to support concurrent incoming A2A context driving concurrent tool execution. The invariant forbids only **re-entering the bridge** (taking `lock(bridge)` again) from any callback or future that contributes to resolving the current stream’s promise. Tool sessions do **not** use the bridge: `openToolSession` → `__tool_session_open` → `context::with_scope` + `baml_manager.open_tool_session` (and send/next/finish/abort) use the tool registry and session state only. So multiple tools can run concurrently within the same stream (or across streams when using multiple bridges) without touching the bridge lock. The only forbidden pattern is: from inside a stream’s `onChatMessage` (or from a tool invoked by it), calling something that needs the **same** bridge—e.g. `invoke_js_function`, `evaluate`, or another stream on that bridge. Use separate bridge instances or avoid A2A-from-inside-tool if you need nested stream-like work.

---

### 5. Non-stream evals: promise resolution observable after running pending jobs

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

### 6. Yield chunk JSON round-trip

**Property:**

- Chunks written in JS via `__chat_yield(chunk)` are stringified in JS and passed to `__baml_chat_yield_host(chunk_json)`. The host parses the JSON to `Value` and pushes to the session’s channel. The collector drains `Value` from the channel. The shape must match what the stream normalizer and pipeline expect.

**Formal:**

- ∀ chunk: JS calls `__baml_chat_yield_host(JSON.stringify(chunk))`; host does `Value = serde_json::from_str(chunk_json)` and `tx.send(Value)`; collector receives `Value` from `drain_yield_buffer(rx)`. No separate eval for buffer read; the channel is the single path.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | `__baml_chat_yield_host` parses one JSON string and sends one `Value`; `drain_yield_buffer` returns `BufferDrain { chunks: Vec<Value>, .. }`. |
| Contract    | Agent yields plain objects (message/task/statusUpdate/artifactUpdate); normalizer accepts them. |

---

### 7. Coordination via globalThis only (no host state in JS)

**Property:**

- `globalThis.__chat_yield` is set in the stream IIFE for coordination only (which host function to call). It is not used to pass host state (e.g. session id). The IIFE may set other globals for coordination (e.g. which native to call), but must not inject tokens or session ids that could be forged or misused by hostile JS.

**Formal:**

- The only stream-related global set by the IIFE is `globalThis.__chat_yield = function(chunk) { __baml_chat_yield_host(JSON.stringify(chunk)); }`. No `__sid`, `__streamSessionId`, or other host-issued token appears in JS.

**Enforcement:**

| Layer        | Mechanism                                                                 |
|-------------|----------------------------------------------------------------------------|
| Application | `start_stream_session` generates the IIFE with no session id in args and no session id in the override; session routing is entirely in the host. |
| Security    | Host state (StreamSessionId, invocation tokens) is never passed into JS for stream yield or tool/baml dispatch; host resolves from LIFO or slot. |

---

## Summary table

| ID | Invariant                              | Violation symptom           | Mitigation                                        |
|----|----------------------------------------|-----------------------------|---------------------------------------------------|
| 1  | Single-active-stream (one permit)      | Multiple streams racing     | Semaphore(1); permit in session, released on finalize. |
| 2  | Host-only session identity             | Host state in JS / escape   | No session id in JS; __baml_chat_yield_host reads slot. |
| 3  | Per-session channel and current slot   | Wrong or missing chunks     | Per-session tx/rx; slot set at start, cleared at finalize. |
| 4  | No bridge re-entrancy during invoke    | Hang (deadlock)             | No A2A session from inside stream handler.       |
| 5  | Promise resolution observable         | Hang (infinite poll loop)   | Run pending jobs before each __eval_result check. |
| 6  | Yield chunk JSON round-trip            | Bad chunks / parse error    | Host parses chunk_json and sends Value to channel. |
| 7  | Coordination via globalThis only       | Host state in JS            | IIFE sets only __chat_yield → host; no tokens in JS. |

## Concurrent stream invariants (summary)

These properties govern the stream/yield boundary and concurrency model:

| ID   | Property | Formal |
|------|----------|--------|
| **S1** | Single permit | ∀ t: at most one stream session holds the semaphore permit. |
| **S2** | Host-only session id | No `StreamSessionId` or session token is ever passed to or from JS. |
| **S3** | Current-stream slot | When a stream runs, `current_stream_session_id_slot = Some(sid)`; `__baml_chat_yield_host` routes to `a2a_yield_tx_by_session[slot]`; on finalize, slot cleared for that sid. |
| **S4** | Per-session channel | Each stream has exactly one (tx, rx); tx in map keyed by session id, rx returned to caller and drained via `drain_yield_buffer(rx)`. |
| **S5** | Coordination only in globalThis | `globalThis.__chat_yield` is set by the IIFE to call the host; it does not carry or store host state (no session id, no tokens). |

**Concurrency:** With one permit (current design), stream invocations are **serialized**: only one stream's JS runs at a time. To allow concurrent streams, the semaphore capacity would be increased and the "current" stream would need to be resolved per execution (e.g. async context or explicit session id from a trusted path); invariants S2 and S5 (no host state in JS) would remain.

## Change applied

- **Invariant 5 (formerly 3):** In `evaluate()`’s promise-polling loop, call `exe_rt_task_in_event_loop(rt.run_pending_jobs_if_any())` **before** evaluating the check script that reads `globalThis.__eval_result`, so the runtime can run promise continuations before we look. This preserves the invariant that “after running pending jobs, __eval_result is visible if the promise has resolved.”

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

**Effects drive provenance**: Tool/LLM executors emit `EffectEvent` (Started/Completed) via `EffectEmitter`. `BusWithEffects` maintains in-flight counts per `ContextId` and fans out to subscribers (e.g. `ProvenanceEffectSubscriber` that converts effects to provenance events).

**Liveness gating**: `QuickJSBridge::evaluate()` checks `EffectLiveness::in_flight(context_id)` in the poll loop:
- If `in_flight(context_id).has_progress_effects()`: use `max_attempts_ms` (configurable, default 30 minutes) — downstream effects are active, progress is possible.
- Otherwise use `idle_timeout_ms` (default 5s, configurable) — no downstream progress effects, likely stuck.
- A2A command envelopes are tracked separately and do not count as progress effects. Only progress-capable effects extend polling.

### Semi-formal properties

| ID | Property | Meaning |
|----|----------|---------|
| **L5** | Effect-gated timeout: if downstream progress effects are in-flight, use long timeout; else use short timeout | Distinguishes "waiting on effect" from "will never yield". |
| **L6** | Completion events always fire: `Started` → `Completed` (success or error) | Prevents "forever in-flight" states. |

**Formal (L5):**

- ∀ poll iteration i in `evaluate()`:
  `timeout_attempts(i) = if in_flight(context_id).has_progress_effects() then MAX_ATTEMPTS else idle_timeout_ms`
- So: when effects are active, we allow longer waits; when idle, we fail fast.

**Formal (L6):**

- ∀ effect emission: if `EffectEvent::ToolStarted { context_id, .. }` or `EffectEvent::LlmStarted { context_id, .. }` is emitted, then eventually `EffectEvent::ToolCompleted { context_id, .. }` or `EffectEvent::LlmCompleted { context_id, .. }` is emitted (even on error).

**Enforcement:**

| Layer | Mechanism |
|------|-----------|
| Execution | Tool/LLM paths emit `Started` before effect, `Completed` in all completion/error paths. |
| Liveness | `QuickJSBridge` queries `EffectLiveness` in poll loop; applies timeout based on `in_flight()`. |
| Scope | Context-only scope: timeout gating uses progress-capable effects for that context; command envelopes are excluded. |

### Implementation

1. **Effect types** (`baml_rt_core::bus`):
   - `EffectKind::{Tool, Llm}`
   - `EffectEvent::{ToolStarted, ToolCompleted, LlmStarted, LlmCompleted}` with metadata
   - `EffectEmitter` trait (async `emit(EffectEvent)`)
   - `EffectLiveness` trait (async `in_flight(context_id) -> InFlightCounts`)

2. **BusWithEffects** (`baml_rt_core::bus::BusWithEffects`):
   - Maintains `HashMap<ContextId, InFlightCounts>` (increments on Started, decrements on Completed)
   - Subscribers receive events (e.g. `ProvenanceEffectSubscriber` converts to `ProvEvent`)

3. **Wiring** (`RuntimeBuilder`):
   - If `provenance_writer` is provided, create `BusWithEffects`
   - Subscribe `ProvenanceEffectSubscriber` to bus
   - Set bus as `EffectEmitter` on `BamlRuntimeManager` and `BamlExecutor`
   - Set bus as `EffectLiveness` on `QuickJSBridge`

4. **Execution sites**:
   - `BamlRuntimeManager::execute_tool()`: emit `ToolStarted` before execution, `ToolCompleted` after (all paths)
   - `baml_pre_execution::intercept_llm_call_pre_execution()`: emit `LlmStarted` before LLM call
   - `BamlLLMCollector::process_trace_events()`: emit `LlmCompleted` after LLM call

5. **Liveness gating** (`QuickJSBridge::evaluate()` poll loop):
   - Query `effect_liveness.in_flight(context_id)` each iteration.
   - **Deterministic sampling:** The first timeout sample is taken only after one run of pending jobs (so the promise executor has had a chance to run and emit effects). Effect state is then re-checked every 10 attempts for the first 500 attempts, then every 100; re-check only ever increases the timeout, never decreases it.
   - Apply `idle_timeout_ms` if no effects, `max_attempts_ms` if effects active.

### Configuration

- `QuickJSConfig::idle_timeout_ms`: Short timeout when no effects (default: 5000ms)
- `QuickJSConfig::max_attempts_ms`: Long timeout when effects are in-flight (default: 1,800,000ms = 30 minutes)
- Set via `RuntimeBuilder::with_quickjs_config(QuickJSConfig::new().with_idle_timeout_ms(Some(ms)).with_max_attempts_ms(Some(ms)))`

### Limitations

- **Context-only scope**: Concurrent requests in the same context will suppress timeouts (acceptable per design).
- **No per-effect tracking**: We track counts, not individual effect IDs (sufficient for liveness gating).
- **Completion guarantee**: Relies on executor correctness (all paths emit Completed); no automatic cleanup on panic.
