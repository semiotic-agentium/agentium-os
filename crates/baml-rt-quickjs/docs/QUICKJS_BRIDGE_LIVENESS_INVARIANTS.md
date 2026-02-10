# QuickJSBridge Liveness Invariants

This document formalizes the liveness and deadlock-prevention invariants for the QuickJSBridge subsystem.

**Related:** For **invocation scope** and why task-local is not visible inside native callbacks (worker-thread EventLoop), see [QUICKJS_THREADING_AND_SCOPE.md](./QUICKJS_THREADING_AND_SCOPE.md).

## Critical Liveness Invariants

### L1: Bridge Initialization Termination

**Property:**
```
∀ initialization sequence init:
  init ∈ {new_with_config, initialize_sandbox, register_baml_functions}:
    init MUST terminate within bounded time T_init
```

Bridge initialization operations must complete within a bounded time. If initialization hangs indefinitely, the system cannot make progress.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Application** | Timeout wrappers around `QuickJSBridge::new_with_config()` and `initialize_sandbox()` |
| **Testing** | Watchdog timeouts in tests (30s for setup, 5s for liveness tests) |
| **Runtime** | QuickJS `runtime.eval()` must be non-blocking and yield to tokio |

**Violation Detection:**
- Tests hang indefinitely (detected by watchdog)
- Production initialization exceeds timeout threshold

### L2: Runtime Eval Non-Blocking

**Property:**
```
∀ code c, ∀ time t:
  runtime.eval(c).await MUST yield control to tokio scheduler within bounded time T_yield
```

QuickJS `runtime.eval()` operations must not block the tokio runtime indefinitely. Each eval must yield control periodically to allow other tasks to progress.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **QuickJS Runtime** | `runtime.eval()` implementation must yield to tokio event loop |
| **Application** | Timeout wrappers around critical eval operations |
| **Testing** | Property tests verify eval operations complete or timeout |

**Potential Violations:**
- Infinite loops in JavaScript code
- Synchronous blocking operations in QuickJS runtime
- Deadlocks in promise resolution

### L3: Lock Acquisition Ordering

**Property:**
```
∀ operations op1, op2 that acquire locks:
  IF op1 acquires locks in order [L1, L2]
  AND op2 acquires locks in order [L2, L1]
  THEN deadlock is possible

THEREFORE: All operations MUST acquire locks in consistent order
```

Lock acquisition must follow a consistent ordering to prevent deadlocks.

**Current Lock Order:**
1. `baml_manager.lock().await` (BamlRuntimeManager)
2. `bridge.lock().await` (QuickJSBridge)
3. `interceptor_registry.lock().await` (InterceptorRegistry)

**Note:** `ToolRegistry` uses a short-lived internal mutex and must not be held across `await`.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Application** | Consistent lock ordering in all code paths |
| **Code Review** | Verify lock order in all async operations |
| **Testing** | Concurrent operation tests to detect deadlocks |

### L4: Promise Resolution Bounded Wait (Non-Stream)

**Property:**
```
∀ promise p in evaluate() WHERE p is NOT a stream request:
  IF p is created by runtime.eval()
  THEN p MUST resolve OR timeout within bounded time T_promise

  T_promise = {
    idle_timeout_ms IF no effects in-flight
    max_attempts_ms IF effects in-flight
  }
```

Promises created during `evaluate()` for non-stream requests must resolve within effect-gated timeouts. The timeout depends on whether effects are in-flight (legitimate wait) vs idle (potential deadlock).

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Application** | `EffectGatedPoller` determines timeout based on in-flight effects |
| **Liveness Tracking** | `effect_liveness` tracks tool/LLM/A2A effects |
| **Testing** | Property tests with mocked liveness verify timeout behavior |

**Violation Detection:**
- Promise resolution exceeds `max_attempts_ms` when no effects in-flight
- Promise resolution exceeds `max_attempts_ms` when effects are in-flight (legitimate wait)

### L6: Stream Promise Non-Termination (By Design)

**Property:**
```
∀ stream request s:
  invoke_js_function_stream(s) starts async execution AND returns immediately
  The promise from onChatMessage() NEVER resolves (by design)
  Chunks are collected via get_a2a_yield_buffer() after invocation
  Promise only completes on agent exit or crash
```

For stream requests, the promise from `onChatMessage()` is DESIGNED to never resolve. It yields chunks via `__baml_chat_yield()` and runs indefinitely until agent termination.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Type system** | `A2aYieldSession<'a, S, NonResolvingPromise>` typestate; only `invoke_js_function_stream()` is used in stream path |
| **Application** | `invoke_js_function_stream()` starts function but does NOT wait for promise resolution |
| **Stream Protocol** | `A2aYieldSession` (with `NonResolvingPromise` marker) uses `invoke_js_function_stream()` only |
| **Trait** | `JsStreamInvoker` trait encodes stream-only invocation contract |
| **Yield Buffer** | Chunks are collected via `get_a2a_yield_buffer()` after invocation completes |
| **Testing** | Stream tests verify chunks are collected without waiting for promise resolution |

**This is NOT a violation - it's the intended design:**
- Stream functions are long-running async generators
- They yield chunks incrementally via `__baml_chat_yield()`
- The promise never resolves because the function never completes (until agent exit)

### L5: Sandbox Initialization Atomicity

**Property:**
```
initialize_sandbox() MUST complete atomically:
  - Either: Sandbox is fully initialized AND console object is available
  - Or: Initialization fails with error (no partial state)
```

Sandbox initialization must be atomic - no partial initialization states that could cause undefined behavior.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Application** | Single `runtime.eval()` call for entire sandbox setup |
| **Error Handling** | Errors propagate immediately, no partial state |
| **Testing** | Verify sandbox state after initialization |

## Deadlock Prevention Rules

### Rule 1: No Nested Lock Acquisition

**Property:**
```
∀ operation op:
  IF op holds lock L1
  THEN op MUST NOT acquire lock L2 that could be held by another operation waiting for L1
```

Prevent circular wait conditions by avoiding nested lock acquisition that could create cycles.

### Rule 2: Lock Release Before Async Yield

**Property:**
```
∀ operation op that acquires lock L:
  op MUST release L before:
    - Calling async function that could acquire another lock
    - Yielding to tokio scheduler (await point)
```

Locks must be released before async operations that could acquire other locks, preventing deadlocks across async boundaries.

**Example Violation:**
```rust
// BAD: Lock held across async boundary
let manager = self.baml_manager.lock().await;
some_async_operation().await; // Could acquire another lock
drop(manager);
```

**Example Correct:**
```rust
// GOOD: Lock released before async operation
let functions = {
    let manager = self.baml_manager.lock().await;
    manager.list_functions()
};
drop(manager); // Explicit release
some_async_operation().await; // Safe - no lock held
```

**Application: `__baml_stream` (BAML function stream run)**

When running a BAML function stream (`stream.run(..., &ctx_manager, ...).await`), the manager lock must be released **before** `stream.run`. Reason: `stream.run` is async and may trigger tool calls (or other work) that need to acquire `baml_manager`; holding the lock across `stream.run` would deadlock. `ctx_manager` is an owned `RuntimeContextManager` created from explicit invocation scope (`create_ctx_manager_for_scope(...)`), so it does not borrow the executor; we can drop the manager guard after obtaining `stream` and `ctx_manager`, then call `stream.run(...).await` without holding any lock.

### Rule 3: Timeout All Blocking Operations

**Property:**
```
∀ blocking operation op:
  op MUST have timeout T_op
  AND timeout MUST be enforced via tokio::time::timeout()
```

All potentially blocking operations must have timeouts to prevent indefinite hangs.

### L7: Promise Polling Timeout Monotonicity (CG6)

**Property:**
```
∀ polling loop P in evaluate():
  timeout_attempts MUST NOT decrease mid-loop
  ∴ P either makes progress OR exits within bounded attempts
```

Re-checking effects periodically must only increase the timeout, never decrease it, to avoid premature timeout when effects complete mid-poll.

**Enforcement:** `timeout_attempts = timeout_attempts.max(new_timeout)` when re-checking; yield and bounded sleep in loop.

## Effect Lifecycle Invariants (CG3 / E1)

### E1: Effect Token Completion

**Property:**
```
∀ token t = EffectStartToken<K>:
  (t.complete() is called) ∨ (t is explicitly abandoned)
  If token is dropped without completion: log error; panic in debug builds
```

**Enforcement:** `EffectStartToken` holds `Option` fields; `complete()` takes them; `Drop` panics/logs if still present.

### E2: Effect Count Underflow Detection

**Property:**
```
∀ Completed event for context c, kind K:
  IF in_flight(c).K == 0 THEN log error (Completed without matching Started)
  Count is still saturating_sub to avoid negative; error is observable in logs
```

**Enforcement:** In `EffectBus::process_event`, before `saturating_sub`, check `entry.tool == 0` (and llm, a2a) and log error.

## Concurrency Guarantees (Summary)

| ID | Property | Enforcement |
|----|----------|-------------|
| CG1 | Yield buffer single writer per session | Bridge lock held for setup→invoke→collect |
| CG2 | Lock order acyclic | Documented lock order; audit |
| CG3 | Effect count consistency | Token discipline; underflow detection |
| CG4 | Stream promise non-resolution | Typestate + `invoke_js_function_stream()` only |
| CG5 | Scheduler fairness | `yield_now()` and bounded sleep in poll loop |
| CG6 | Async task progress bound | Monotonic timeout; loop terminates |

## Current Violations and Fixes

### Fix: Stream Promise Non-Termination (By Design)

**Symptom:** `test_a2a_jsonrpc_request_invokes_js_function` hangs indefinitely in `invoke_js_function()` when calling `onChatMessage()`.

**Root Cause Analysis:**
1. `invoke_js_function()` wraps the call in `__awaitAndStringify(func(args))` which returns a promise
2. `evaluate()` wraps that promise in an async IIFE that awaits it and sets `__eval_result`
3. The promise polling loop waits for `__eval_result` to be set
4. **The promise from `onChatMessage()` never resolves BY DESIGN** - it's a stream function that yields chunks
5. Stream functions are long-running async generators that never complete until agent exit

**Fix Applied:**
- Created `invoke_js_function_stream()` that starts the async function but does NOT wait for promise resolution
- Updated `A2aYieldSession::invoke()` to use `invoke_js_function_stream()` instead of `invoke_js_function()`
- Documented **INVARIANT L6** that stream promises are designed to never resolve
- Added timeouts around critical operations (`initialize_sandbox`, `register_baml_functions`, `init_js` evaluation)

**Design Rationale:**
- Stream requests use yield-based protocol: `__baml_chat_yield(chunk)` pushes chunks to buffer
- The promise never resolves because the function runs indefinitely (until agent exit)
- Chunks are collected via `get_a2a_yield_buffer()` after invocation, not from promise resolution
- This is the intended design, not a bug

## Testing Strategy

### Property Tests

1. **L1 Test:** Bridge initialization completes within timeout
2. **L2 Test:** Eval operations yield control periodically
3. **L3 Test:** Concurrent operations don't deadlock
4. **L4 Test:** Promise resolution respects effect-gated timeouts
5. **L5 Test:** Sandbox initialization is atomic

### Watchdog Tests

All liveness-related tests must have watchdog timeouts:
- Setup operations: 30 seconds
- Liveness property tests: 5 seconds
- Integration tests: Configurable per test

## Recommendations

1. **Add timeout to `initialize_sandbox()`:**
   ```rust
   timeout(Duration::from_secs(5), bridge.initialize_sandbox()).await?
   ```

2. **Add timeout to `register_baml_functions()`:**
   ```rust
   timeout(Duration::from_secs(10), bridge.register_baml_functions()).await?
   ```

3. **Verify QuickJS runtime yields:**
   - Check `runtime.eval()` implementation
   - Ensure it calls `tokio::task::yield_now()` periodically
   - Or verify it's truly non-blocking

4. **Add deadlock detection:**
   - Track lock acquisition order
   - Detect circular wait conditions
   - Log warnings when potential deadlocks detected
