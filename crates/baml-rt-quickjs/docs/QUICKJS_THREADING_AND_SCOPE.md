# QuickJS Runtime Threading and Invocation Scope

This document describes how the **quickjs_runtime** crate runs JavaScript and why **invocation scope** (request-scoped context) must be provided in a way that is visible to native callbacks. This is a **common requirement** for any code that runs request-scoped logic (provenance, effects, context_id) from JS-triggered native callbacks.

## QuickJS Runtime Threading Model

From the [quickjs_runtime](https://docs.rs/quickjs_runtime/latest/quickjs_runtime/) docs:

- **Worker-thread EventLoop:** All QuickJS API calls are directed to a **single worker thread** (the EventLoop). You call `rt.eval(...).await` or `rt.loop_realm(...).await` from any thread; the runtime **posts a job** to the worker thread, which then runs the QuickJS API (including `eval` and any native callbacks).
- **Adapters are thread-bound:** `QuickJsRuntimeAdapter` and `QuickJsRealmAdapter` (and thus native callbacks registered with `set_function`) **run only on the worker thread**. They are not `Send` and must not leave that thread.
- **Facades are thread-safe:** `QuickJsRuntimeFacade`, `JsValueFacade`, etc. can be used from any thread; they communicate with the worker thread via the event loop.

So when we do:

```text
Tokio task T (handling request R):
  context::with_scope(scope_R, async {
    bridge.invoke_js_function(scope_R, "handle_a2a_request", args).await
      → with_scope(scope_R, async { self.runtime.eval(script).await })
  })
```

the **eval** is submitted to the QuickJS worker thread. When the evaluated JS calls a native function (e.g. `__tool_invoke`), that **callback runs on the worker thread**, not on the Tokio task T. Therefore:

- **`context::current_scope()` (task-local) is set on the Tokio task.**  
- **Native callbacks run on the QuickJS worker thread.**  
- **Task-local is per-Tokio-task; the worker thread is a different execution context.**  
- So **`current_scope()` is `None` inside native callbacks** when they run on the worker thread.

## Common Requirement: Scope Visible in Native Callbacks

Any design that needs **request-scoped context** (e.g. `context_id`, provenance, effects) inside **native callbacks** (e.g. `__tool_invoke`, `__tool_session_open`, `__baml_invoke`) must satisfy:

**Requirement:** The scope for the **current invocation** must be available to the code that runs **on the QuickJS worker thread** when the callback executes.

- **Not sufficient:** Setting scope only via `context::with_scope(scope, ...)` on the Tokio task and then calling `current_scope()` inside the native callback. The callback runs on the worker thread; task-local is not visible there.
- **Needed:** Scope (or a way to get it) must be provided to the **worker thread** for the duration of the eval that can trigger that callback. Options include:
  1. **Per-eval context in the runtime:** If the runtime supports passing an “invocation context” (e.g. `eval(script, context)`) that it sets on the worker thread before running the script and that native callbacks can read, use that.
  2. **Bridge-held “current scope” for the worker:** Before submitting the eval job, the bridge stores the current invocation scope in a place the worker thread can read (e.g. a thread-local set by the runtime when it starts running the script, or a shared slot that the runtime sets from the calling thread and the worker reads). Native callbacks then read scope from that place instead of from task-local.
  3. **Scope captured in the script/callback registration:** If the runtime allows passing data into the eval job that is visible to the worker during that job, scope can be passed with the job and stored in a worker-visible location for the duration of that eval.

Until one of these is implemented, **do not rely on `context::current_scope()` inside native callbacks** when the runtime uses a separate worker thread; it will be `None` and can panic or mis-attribute context.

## Thunking Call Context via quickjs_runtime (No JS)

**quickjs_runtime** does not pass a “context” parameter into `eval`. It does expose **`loop_realm(realm_name, closure)`**: the closure runs **on the worker thread** and receives `(QuickJsRuntimeAdapter, QuickJsRealmAdapter)`. Internally, `eval(realm_name, script)` is implemented as:

```rust
self.loop_realm(realm_name, |_rt, realm| {
    let res = realm.eval(script);
    // ... convert to JsValueFacade
})
```

So we can **thunk call context** without touching JavaScript:

1. **Worker-thread thread-local:** In the bridge (or core context) define a **thread-local** (e.g. `WORKER_INVOCATION_SCOPE: RefCell<Option<RuntimeScope>>`) that native callbacks read. Only the QuickJS worker thread runs native callbacks, so this thread-local is exactly the worker thread’s “current invocation scope.”
2. **Use `loop_realm` instead of `eval` when scope is present:** For any eval that can trigger native callbacks and that has an `InvocationScope`, call `runtime.loop_realm(None, move |_rt, realm| { ... })` with a closure that:
   - Captures the invocation scope (e.g. `scope.as_scope().clone()`).
   - Sets the thread-local to `Some(scope)` (and restores the previous value when done, e.g. with a guard).
   - Calls `realm.eval(script)` and converts the result to `JsValueFacade` as `eval` does.
   - Restores the thread-local (or let the guard do it).
3. **Native callbacks:** Use a helper (e.g. `context::worker_thread_scope() -> Option<RuntimeScope>`) that reads the thread-local. No JS-passed context_id or global is required.

**Benefits:** Scope is set and read entirely on the Rust side; no `globalThis.__baml_context_id` or JS arguments for context; same API for JS (no changes to how JS calls `__tool_invoke`, etc.). Any code path that runs eval with an `InvocationScope` (e.g. `invoke_js_function(scope, ...)`, `invoke_js_function_stream(scope, ...)`) must use this “eval with scope” thunk for every `eval` in that path (including the promise-poll loop in `evaluate()`).

**Stream + concurrency:** For stream requests we leave the worker-thread scope set (`clear_after: false`) so async promise continuations (e.g. `openToolSession` → `__tool_session_open`) see it; we clear in `get_a2a_yield_buffer`. When **multiple** stream requests run concurrently, each `run_eval_with_scope(..., false)` overwrites the single thread-local. So the last request’s scope wins, and earlier requests’ async continuations can see the wrong scope.  
To support **concurrent streams without per-request runtimes**, **do not rely on the global token**. Use explicit token passing in JS (e.g. `openToolSession(toolName, invocationToken)`), and keep authoritative scope resolution on the host via the token → scope map. This avoids global token collisions and removes reliance on the worker-thread scope for attribution.

---

## Passing Context via JavaScript: Host Design and Trust

When we pass context through JavaScript (e.g. so each async continuation can carry its own scope under concurrency), **JS may be dysfunctional**: buggy code, malicious code, or shared globals overwritten by another task. The host must be designed so that **JS cannot corrupt authoritative attribution**.

### Can the closure be corrupted from within?

Yes. In practice “passing context via JS” means the host injects a value (e.g. `context_id` or a token) into the JS environment (prelude, closure, or arguments). When JS later calls a native (e.g. `__tool_invoke(context_id, ...)`), it passes that value as an argument. **JS can pass any value**: it can use a different `context_id`, overwrite the closed-over variable before calling the native, or call the native with a forged or stale value. So the host **must not trust** the JS-passed value for authoritative provenance/effects. Treat it as untrusted input.

### Host design principles

1. **Never trust JS-passed context for authoritative attribution.** For provenance events and effect attribution, the host must not rely solely on a value that JS supplies. Either:
   - **Opaque token:** When the host starts an invocation, it creates a unique **invocation token** (e.g. a nonce or index) and stores `token → scope` in a host-held map. The host injects only the **token** into JS (e.g. in the prelude or closure). When a native callback runs, it receives the token from JS and **looks up** the scope in the host map. JS cannot forge a valid token for another request; it can only pass back the token it was given (or an invalid value). Attribution is then **host-authoritative**.
   - **Validate:** The host maintains the set of context_ids (or tokens) that are valid for the current bridge/session. When a native runs with a JS-passed value, the host **validates** it (e.g. “is this context_id one we issued for an active invocation?”). If invalid, treat as unknown or reject; do not attribute to an arbitrary JS-supplied context_id.
2. **Defence in depth:** Even with tokens, cap the lifetime of entries in the token map (e.g. remove when the invocation completes or after a timeout) so stale or leaked tokens cannot be reused indefinitely.
3. **Document the contract:** JS is instructed to pass the value it was given (e.g. “pass `__baml_context_id` as the first argument”). The host still does token lookup or validation; the contract is for correct behaviour, not for security.

### Summary

| Approach              | Who provides context at callback time | Can JS corrupt attribution? | Host design |
|-----------------------|--------------------------------------|-----------------------------|-------------|
| Worker-thread thunk   | Host (thread-local set before eval)  | No                          | No trust of JS. |
| JS-passed raw context_id | JS (closure/args)                | Yes                         | Do **not** use for authoritative attribution. |
| JS-passed opaque token | JS passes token; host looks up scope | No (host looks up)          | Token → scope map; validate or reject invalid token. |

So: **passing context via JS is acceptable for concurrency (e.g. each continuation carries a token), but the host must resolve that token (or validate the value) and never attribute solely on the basis of an unvalidated JS-supplied context_id.**

## Stream concurrency design note (current)

- **Token required:** `openToolSession` **requires an explicit token** (`openToolSession(toolName, token)`); there is **no global fallback** and **no fallback to worker-thread scope** for attribution—natives resolve scope only via the token → scope map.
- For **concurrent stream requests**, JS must pass the per‑invocation token explicitly (e.g. via `args.__baml_invocation_token`); global token state is not used.
- **Stream semaphore:** Only one stream invocation may be active per bridge at a time (a semaphore is acquired in `invoke_js_function_stream` and released in `get_a2a_yield_buffer`), so token/scope state is not overwritten by concurrent streams.

## References

- [quickjs_runtime crate docs](https://docs.rs/quickjs_runtime/latest/quickjs_runtime/): worker-thread EventLoop, facades vs adapters.
- [CONTEXT_CONCURRENCY_INVARIANTS.md](./CONTEXT_CONCURRENCY_INVARIANTS.md): invariants (I1–I3) and single source of truth for context; enforcement must respect the threading model above.
- [QUICKJS_BRIDGE_LIVENESS_INVARIANTS.md](./QUICKJS_BRIDGE_LIVENESS_INVARIANTS.md): eval non-blocking, lock ordering, promise resolution.
