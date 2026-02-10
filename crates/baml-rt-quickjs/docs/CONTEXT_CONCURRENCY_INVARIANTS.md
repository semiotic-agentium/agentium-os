# Context Concurrency Invariants

This document states the invariants required for **request-scoped context** under concurrent A2A/tool handling, and how the design enforces them. The test `test_context_id_is_task_local_under_concurrency` asserts these invariants.

## Invariants (What the Test Requires)

### I1: Request-scoped attribution

**Property:** For each A2A request R with `context_id` C, every provenance event (and effect) that is logically part of handling R must be attributed to C.

- All tool starts/completes for that request must carry C.
- No event for request R may be attributed to another request’s `context_id`.

### I2: Session tool invocation count

**Property:** Each session-based tool use (open + send/continue) produces exactly two logical tool invocations: open and execute. Each has one start and one complete.

- Open: one `ToolCallStarted`, one `ToolCallCompleted`.
- Execute (send + next): one `ToolCallStarted`, one `ToolCallCompleted`.

### I3: No cross-request attribution under concurrency

**Property:** When multiple requests are handled concurrently (e.g. 8 tasks on a `LocalSet`), no event for request R1 may be attributed to R2’s `context_id`, and vice versa.

- The only way to satisfy I1 and I3 is to bind context to the **request/task** and never derive it from shared mutable state (e.g. JS globals) at the time of a native callback.

### I4: Tool session scope retention

**Property:**

```text
∀ session s created by open_tool_session:
  s.id ∈ tool_session_scopes
  from (open returns) until (finish/abort) or (send error removes s)
```

Every session ID returned by `open_tool_session` must remain in `tool_session_scopes` until the session is explicitly closed (finish/abort) or a send error removes it. `tool_session_send/next/finish/abort` require this mapping to enforce per-request attribution.

## Single Source of Truth for Context

- **Transport:** For each request, the transport builds `scope = RuntimeScope::new(request_context_id, ...)` from the parsed request and runs the entire handler under `context::with_scope(scope, route()).await`. So for the **duration** of handling that request, the task’s `RUNTIME_SCOPE` is that request’s scope.
- **tokio::task_local!(RUNTIME_SCOPE):** Scope is **per-task**. Each spawned task has its own `RUNTIME_SCOPE`. So when task T is handling request R, only T’s scope is R’s scope; other tasks have their own scopes.
- **Bridge / native callbacks:** The only concurrency-safe source of scope for native callbacks is “which request is this?” is **token lookup**: the host issues a token per eval, stores `token → scope`, and natives resolve scope from the token (passed by JS). There is **no** task-local fallback in native callbacks (they run on the worker thread); invalid or missing token returns an error.

## QuickJS Threading Constraint (Common Requirement)

**quickjs_runtime** runs all JS (and native callbacks) on a **worker-thread EventLoop**, not on the Tokio task that called `eval`. So **task-local scope is not visible inside native callbacks**—they run on the worker thread, where task-local lookup returns `None`. Any design that needs request-scoped context inside native callbacks must provide scope in a way the **worker thread** can read (e.g. per-eval context, bridge-held scope for the worker). See **[QUICKJS_THREADING_AND_SCOPE.md](./QUICKJS_THREADING_AND_SCOPE.md)** for the threading model and options.

## Where Invariants Can Be Violated

| Location | Risk | Enforcement |
| --- | --- | --- |
| **Bridge native entry points** that take `context_id` from JS args | JS may pass a value that was overwritten by another task, or JS may be buggy/malicious and pass a wrong or forged value. | **Never trust JS-passed context_id for authoritative attribution.** When passing context via JS (e.g. for concurrent streams), use an **opaque token** (host issues token, stores token→scope, native looks up) or **validate** JS-passed value against a host-held set. See [QUICKJS_THREADING_AND_SCOPE.md](./QUICKJS_THREADING_AND_SCOPE.md) § Passing context via JavaScript. |
| **open_tool_session** storing scope for later use in send/next | If scope is read **after** an `await`, task-local might have changed (wrong task). | Pass scope explicitly to `open_tool_session` and store that value in `ToolSessionScope` before any async boundary. |
| **tool_session_send / tool_session_next** | Must run under the **same** scope as the open that created the session. | Store scope at open; run send/next inside `context::with_scope(stored_scope, run()).await`. Never use `current_or_new()` for send/next without that wrapper. |
| **ProvenanceInterceptor** | Must see the same context_id for start and complete for a given call. | All call sites (open, send, one-shot execute) build `ToolCallContext` from the scope in effect for that call (task-local or stored scope). |

## Enforcement Rules (Design)

1. **Transport:** Set scope once per request from parsed `request.context_id`; run entire `route()` inside `with_scope(scope, route()).await`. No other code in that path may replace scope for that request.
2. **Bridge:** Native callbacks **do not** use task-local context (worker thread has no task-local). Scope is resolved **only** from the **token → scope** map: the first argument must be a valid invocation token; the host looks up `RuntimeScope` and runs the callback body inside `context::with_scope(scope, ...).await`. No fallback to task-local or JS-passed raw `context_id` for attribution.
3. **BamlRuntimeManager::open_tool_session:** Require explicit `scope` parameter and use its `context_id` for open attribution; store it in `ToolSessionScope` for send/next.
4. **BamlRuntimeManager::open_tool_session:** Reject calls that do not provide scope so we never store a session with missing scope. That way send/next always run inside `with_scope(stored_scope, ...)` and never attribute to the wrong request.
5. **BamlRuntimeManager::tool_session_send / tool_session_next:** Run the inner logic inside `context::with_scope(session_scope.scope, run()).await` when `session_scope.scope` is `Some`; the `require scope` rule for open ensures valid sessions always have `Some(scope)`.
6. **No retries in tests:** Tests that assert these invariants must not use retries; any failure indicates a real concurrency bug.

## Bridge Entry Points: Token Lookup Only

All of these receive a **token** (or token + other args) from JS and resolve scope **only** via the host-held token → scope map. There is **no task-local fallback** (native callbacks run on the worker thread). Invalid or missing token returns an error.

- `__tool_session_open` (token, toolName)
- `__tool_invoke` (token, toolName, args)
- `__tool_from_baml_result` (token, baml_result_json)
- `__baml_invoke` (token, function_name, args)
- `__baml_stream` (token, function_name, args)

Any new native that needs request-scoped attribution must take a token and use the same lookup.
