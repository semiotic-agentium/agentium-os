# baml-rt-quickjs

QuickJS-backed runtime host for BAML execution, with host-managed
context propagation for concurrent tool and A2A flows.

## Responsibilities

- `BamlRuntimeManager` orchestration for schema loading and function execution.
- `QuickJSBridge` integration to expose BAML functions and tools to JavaScript.
- Host-side scope/context propagation and JS value conversion utilities.

## Architecture at a Glance

- **QuickJSBridge** owns the runtime and registers JS helpers (`__baml_invoke`,
  `__tool_invoke`, `__baml_stream`, tool-session helpers).
- **Invocation context frames** are entered per eval/invoke and stored in a
  host-managed active stack.
- **Native callbacks** resolve scope from the active host context; JS never supplies raw
  context for authoritative attribution.

## Context Propagation (Critical Design)

QuickJS executes JS and native callbacks on a **worker-thread event loop**.
Tokio task-local context is not visible there, so the host must supply
request-scoped context explicitly.

### Invariants

- **No shared global state for attribution.** JS globals are shared and unsafe
  under concurrency.
- **Host context is the source of truth.** All native callbacks resolve
  `RuntimeScope` from the active invocation context.
- **Tool sessions keep scope until close.** Session scope stays in the host map
  until finish/abort (or send error).

### Host-Managed Context Flow

1. Host enters an invocation context frame for each eval/invoke.
2. Native helpers resolve scope from the current active host context frame.
3. JS calls helpers directly (e.g. `openToolSession(toolName)`), without
   passing invocation tokens.
4. Native callbacks run with resolved scope attribution in
   `context::with_scope(scope, ...)`.

See `docs/QUICKJS_THREADING_AND_SCOPE.md` and
`docs/CONTEXT_CONCURRENCY_INVARIANTS.md` for detailed guarantees.

## Tool Session Contract

Host tools are invoked from BAML via `ToolSessionPlan` steps. The runtime
executes the session FSM in Rust and returns the final result to JS. JS tools
remain callable via `invokeTool`.

### openToolSession

`openToolSession(toolName)` resolves scope from active host context and does
not require invocation token arguments in JS.

## Streaming (A2A)

- The host invokes `onChatMessage(message)` for each inbound message. The A2A
  shim (injected by the builder) provides `session(message)` and
  `__chat_register({ run })` so agent code uses a single `run(ctx)` entrypoint
  or `session(message).run(...)` with `ctx.emit` / `emit` for messages,
  artifacts, and `awaitInput`.
- A2A streaming uses `__chat_yield` (shim) and a host-set yield buffer; chunks
  are read via `get_a2a_yield_buffer`. The stream handler typically does not
  terminate; chunks drive task state (WORKING, INPUT_REQUIRED, COMPLETED, etc.).
- A **stream semaphore** (one permit per bridge) ensures only one stream
  invocation is active at a time, so token/scope state is not overwritten by
  concurrent streams.

**Promise polling:** For non-stream evals that return a promise, the host uses
effect-gated timeout with deterministic sampling: the first timeout sample is taken
after one run of pending jobs; effect state is re-checked every 10 attempts for the
first 500ms, then every 100. Long timeout when effects are in-flight, short idle
timeout otherwise. See `docs/HOST_QUICKJS_STREAM_INVARIANTS.md` and
`src/quickjs_bridge/promise_polling.rs`.
