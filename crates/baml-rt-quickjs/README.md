# baml-rt-quickjs

QuickJS-backed runtime host for BAML execution, with explicit token-based
context propagation for concurrent tool and A2A flows.

## Responsibilities

- `BamlRuntimeManager` orchestration for schema loading and function execution.
- `QuickJSBridge` integration to expose BAML functions and tools to JavaScript.
- Token-based context propagation and JS value conversion utilities.

## Architecture at a Glance

- **QuickJSBridge** owns the runtime and registers JS helpers (`__baml_invoke`,
  `__tool_invoke`, `__baml_stream`, tool-session helpers).
- **Invocation tokens** are issued per eval and stored in a host-held
  `token -> RuntimeScope` map. JS only sees the opaque token and passes it back
  to native helpers.
- **Native callbacks** resolve scope via the token map; JS never supplies raw
  context for authoritative attribution.

## Context Propagation (Critical Design)

QuickJS executes JS and native callbacks on a **worker-thread event loop**.
Tokio task-local context is not visible there, so the host must supply
request-scoped context explicitly.

### Invariants

- **No shared global state for attribution.** JS globals are shared and unsafe
  under concurrency.
- **Token is the single source of truth.** All native callbacks resolve
  `RuntimeScope` from the token map.
- **Tool sessions keep scope until close.** Session scope stays in the host map
  until finish/abort (or send error).

### Token Flow

1. Host creates `InvocationToken` for each eval and stores `token -> scope`.
2. Prelude binds `const __baml_invocation_token` for the eval scope.
3. JS passes the token explicitly to native helpers (e.g.
   `openToolSession(toolName, args.__baml_invocation_token)` or
   `args.__baml_invocation_token`); the prelude binds it only for the eval
   scope, so registered JS tools must use the token from `args` for nested
   calls.
4. Native callbacks resolve scope from the map and run inside
   `context::with_scope(scope, ...)`.

See `docs/QUICKJS_THREADING_AND_SCOPE.md` and
`docs/CONTEXT_CONCURRENCY_INVARIANTS.md` for detailed guarantees.

## Tool Session Contract

Host tools are invoked from BAML via `ToolSessionPlan` steps. The runtime
executes the session FSM in Rust and returns the final result to JS. JS tools
remain callable via `invokeTool`.

### openToolSession

`openToolSession(toolName, token)` **requires the invocation token** and does
not fall back to globals. JS wrappers pass `__baml_invocation_token` or attach
`args.__baml_invocation_token` so nested calls keep attribution intact.

## Streaming (A2A)

- A2A streaming uses `__baml_chat_yield_buffer` and `__baml_chat_yield`.
- Stream invocations start an async handler but do **not** await its promise to
  completion; the stream handler typically does not terminate. Chunks are read
  from the yield buffer via `get_a2a_yield_buffer` after the host decides to
  stop waiting (e.g. after reading available chunks).
- A **stream semaphore** (one permit per bridge) ensures only one stream
  invocation is active at a time, so token/scope state is not overwritten by
  concurrent streams.

See `docs/HOST_QUICKJS_STREAM_INVARIANTS.md` for liveness and sequencing rules.
