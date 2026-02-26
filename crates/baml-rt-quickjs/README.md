# baml-rt-quickjs

QuickJS-backed runtime host for BAML execution with host-authoritative context,
strict stream/session routing, and resumable A2A stream handling.

## Responsibilities

- `BamlRuntimeManager` orchestration for schema loading and function execution.
- `QuickJSBridge` integration to expose BAML functions and tools to JavaScript.
- Host-managed context/session attribution across concurrent tool and A2A flows.

## Runtime Model

- QuickJS code and native callbacks execute on a worker-thread event loop.
- Tokio task-local scope is not visible inside worker-thread native callbacks.
- Authoritative attribution therefore lives in host-managed structures:
  - invocation context registry,
  - stream session maps,
  - token-to-scope mappings.

## Canonical Architecture

### Bridge Core

- `QuickJSBridge` owns runtime setup, helper registration, and stream/session maps.
- Per-eval invocation context is entered/exited through host registry frames.
- Native callbacks resolve scope from host state; they do not trust JS-provided
  context identifiers.

### Stream Handover

- Production stream path:
  - `spawn_stream_handover(...)`
  - `run_stream_on_js_thread(...)`
  - `collect_into_channel_owned(...)`
- Stream handover is dispatched through a single typed lane queue
  (`StreamHandoverRequest`) rather than ad hoc closures.
- Collector is completion-driven (`SemanticFinal`, `InputRequired`,
  `ChannelClosed`, `Timeout`), not promise-resolution-driven.

### Resume Path

- Resume delivery path:
  - `deliver_resume_input(...)`
  - `prepare_brief_poll_eval(...)`
  - `run_prepared_brief_poll_eval(...)`
  - bounded poll for settle signal
- Resume wait is bounded; stream completion remains governed by stream outputs
  and terminal states.

## Invariant Set (Current)

### Context and Attribution

- Host-only context authority; no JS global mutable state for attribution.
- No fallback routing from ambient mutable state.
- Missing/invalid routing metadata is handled deterministically; no heuristic
  reroute.
- Tool session scope retention: scope captured at open and reused for send/next.

### Stream and Session

- Single production handover path.
- Per-session chunk isolation by `StreamSessionId`.
- Bounded collector loop and single terminalization.
- No global JS helper mutation on stream finalization.
- Stream routing is host-authoritative (`__session` + host session map).

### Liveness and Deadlock Discipline

- Bridge lock is not held across awaited resume poll operations.
- Promise polling is effect-gated with monotonic timeout behavior.
- Poll loop drives pending jobs before waiting, so worker continuations can run.
- Blocking operations are bounded by explicit caps/timeouts.

## A2A Shim Resume Semantics

- Shim registers `session(message)` and `__chat_register(...)`.
- `awaitInput(prompt)` stores pending resolver aliases for both task and context
  session identities, so resume turn routing remains stable.
- Resume message resolves pending input for the same logical session, then
  execution continues in the same stream conversation lifecycle.

## Tool Session Contract

- Host tools are invoked from BAML via `ToolSessionPlan` FSM steps.
- Runtime executes tool session FSM in Rust and returns final result to JS.
- JS tools remain callable via `invokeTool`.

## QuickJS Config Tuning

`QuickJSConfig` (and `quickjs_runtime::builder::QuickJsRuntimeBuilder`) expose:

| Option | Effect | Tuning |
|--------|--------|--------|
| `memory_limit` | Hard cap on runtime heap | Set in production to avoid OOM |
| `max_stack_size` | Max JS stack size | Raise only for deep recursion needs |
| `gc_threshold` | Allocation-based GC trigger | Higher = throughput, lower = lower peak memory |
| `gc_interval` | Timer-based full GC | Optional; use carefully for pause/throughput tradeoff |

QuickJS has no JIT in the standard build; main levers are memory and GC policy.
