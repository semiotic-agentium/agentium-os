# A2A Session & Runtime: Channel-Based Design for MT and High Concurrency

## Goals

1. **Multi-threaded (MT) runtime** – No `LocalSet`, no thread-per-agent; all work runs on the tokio MT runtime.
2. **High agent concurrency** – Scale to many agents; eventually support multiple QuickJS runtimes (one or more workers per runtime).
3. **Explicit message passing** – All coordination via channels; no shared mutable state across tasks except through message boundaries.
4. **Context in messages** – All runtime context (scope, context_id, etc.) is carried **in channel message payloads**, not via task-local storage.

## Principles

- **No task-local for session/request context.** Callers pass scope (or equivalent) in the message. Handlers receive scope in the message and run `with_scope(scope, ...)` locally; no task-local lookup in the hot path.
- **One logical channel per “owner” of a QuickJS runtime.** A runtime worker task owns one runtime and one input channel; it processes messages sequentially for that runtime (or with internal concurrency only where safe). Many agents can share one runtime (one channel) or each agent can have its own runtime (one channel per agent).
- **Session identity is explicit.** Session open returns `session_id`; all subsequent messages (send, next, finish, abort) carry `session_id`. Scope is stored at open and looked up by `session_id` when processing send/next.

---

## Message Boundaries

### 1. Runtime worker (bridge owner)

A **runtime worker** is a tokio task that:

- Owns one QuickJS runtime (bridge).
- Receives messages on a single input channel (e.g. `mpsc::UnboundedReceiver<RuntimeWorkerMsg>`).
- Processes one message at a time (or with a bounded concurrency model) and sends results on channels provided in the message.

**Message type (conceptual):**

```text
RuntimeWorkerMsg =
  | HandleA2a { scope: RuntimeScope, request: Value, response_tx: Sender<Vec<Value>> }
  | ToolInvoke { scope: RuntimeScope, tool_name, input, response_tx }
  | ... (future: other ops that need the bridge)
```

- **Context in message:** Every variant carries `scope`. The worker runs the op inside `context::with_scope(scope, ...)` and does not use task-local.
- **Result:** Sent on `response_tx` (or similar) provided in the message. No task-local on the caller side either; caller gets scope from its own context and puts it in the message.

### 2. A2A session dispatcher

The **session dispatcher** is an MT-safe task that:

- Receives session commands on a single channel: `DispatcherMsg`.
- Maintains a map `session_id -> (scope, response_tx)` populated at **Register** (open).
- For **Send(session_id, request)** it looks up `(scope, response_tx)`, then sends to the **runtime worker**: `HandleA2a { scope, request, response_tx }`.
- No LocalSet; no thread-per-agent. The dispatcher and the runtime worker are both normal tokio tasks (can run on any worker thread).

**Message type (conceptual):**

```text
DispatcherMsg =
  | Register { session_id: ToolSessionId, scope: RuntimeScope, response_tx: Sender<Value> }
  | Cmd { session_id: ToolSessionId, cmd: SessionCmd }

SessionCmd =
  | Send(Value)
  | Finish
  | Abort(Option<String>)
```

- **Context in message:** `Register` carries `scope`; it is stored keyed by `session_id`. Later `Cmd(Send(request))` does not need scope in the payload; the dispatcher looks it up and forwards `(scope, request, response_tx)` to the runtime worker.
- **Ordering:** Single channel per dispatcher so that for a given `session_id`, `Register` is always processed before `Cmd(Send(...))`.

### 3. Tool session open/send/next (registry & BamlRuntimeManager)

Historically, **open_tool_session** (and send/next) depended on task-local scope lookup. In the channel-based design:

- **Open:** The **caller** obtains a scope (e.g. from its request or from a prior message) and passes it **in** the call. For example:
  - `open_tool_session(scope, tool_name, open_input)` (scope in the API), or
  - `open_tool_session(tool_name, open_input)` where `open_input` includes a serialized or reference to scope (e.g. `context_id` + agent_id etc.) and the registry resolves that to a `RuntimeScope` and stores it by `session_id`.
- **Send / Next / Finish / Abort:** These already carry `session_id`. The registry (or the component that executes the tool) looks up the stored scope for that `session_id` and runs the tool with that scope (e.g. `with_scope(stored_scope, execute_send(...))`). No task-local read in the hot path.

So:

- **Context in messages:** Scope is provided at open (either as an argument or inside open_input). It is stored in a map keyed by `session_id` and retrieved when processing send/next/finish/abort. All context flow is explicit via data (open payload + session_id) and channel messages.

---

## Data flow (A2A session, high level)

1. **Open session (caller → registry → dispatcher)**
   Caller has `scope`. It calls something like `open_tool_session(scope, "a2a/session", ())`.
   - Registry creates `session_id`, creates `(response_tx, response_rx)`, sends **Register { session_id, scope, response_tx }** to the A2A session dispatcher, returns `session_id` to caller.
   - No task-local; scope is in the open call and in the Register message.

2. **Send (caller → dispatcher → runtime worker)**
   Caller sends **Cmd { session_id, Send(request) }** to the dispatcher.
   - Dispatcher looks up `(scope, response_tx)` for `session_id`, then sends **HandleA2a { scope, request, response_tx }** to the runtime worker.
   - Runtime worker receives the message, runs `with_scope(scope, handle_a2a(request))`, sends `Vec<Value>` on `response_tx`.
   - Caller is already waiting on `response_rx.recv()` (from the session’s `next()`). So context flowed: caller → Register(scope) → dispatcher map → HandleA2a(scope) → worker.

3. **Next**
   Caller’s `next()` is just receiving on the per-session `response_rx`; no context needed in the message.
   **Finish / Abort:** Dispatcher receives `Cmd { session_id, Finish }` (or Abort), removes `session_id` from the map, drops the `response_tx` so the caller’s `response_rx` gets closed.

All runtime context is carried either in the open call (scope) or in the Register/HandleA2a messages; no task-local.

---

## Runtime worker and bridge affinity

- The **runtime worker** task is the only task that calls into the QuickJS bridge for that runtime. So bridge affinity is “whoever holds the channel sender to that worker.” Many agents (many session dispatchers) can send **HandleA2a** messages to the **same** runtime worker (shared runtime); or each agent can have its own runtime and its own worker (one channel per agent).
- No thread-per-agent: the runtime worker is a single tokio task (or a pool of tasks each owning one runtime). It can run on any MT worker thread; the important invariant is “one task owns one bridge” (or one set of bridges), not “one OS thread per agent.”

---

## Summary table

| Concern              | Current (to remove)              | Target (channel-based)                                      |
|----------------------|-----------------------------------|-------------------------------------------------------------|
| Where handler runs   | LocalSet / same thread as caller  | Runtime worker task (any MT thread); receives HandleA2a msgs |
| Scope for session    | task_local at open/send/next      | Scope in open (and in Register); stored by session_id      |
| Scope for handle_a2a | Built from request in handler    | Carried in HandleA2a message; worker runs with_scope(scope, …) |
| Concurrency          | Single thread / LocalSet          | MT; many sessions, many agents; N runtime workers          |
| Context flow         | Task-local + with_scope in handler| Explicit: open(scope) → Register(scope) → HandleA2a(scope)  |

---

## Implementation notes (for later)

1. **A2A session dispatcher**
   - Replace LocalSet-based worker with a single `tokio::spawn` loop: `while let Some(msg) = rx.recv().await { ... }`.
   - `Register { session_id, scope, response_tx }` → insert into `HashMap<ToolSessionId, (RuntimeScope, mpsc::UnboundedSender<Value>)>`.
   - `Cmd { session_id, Send(request) }` → look up `(scope, response_tx)`, then send to runtime worker: `HandleA2a { scope, request, response_tx }`.

2. **Runtime worker**
   - New task (or reuse existing “bridge owner” concept): receives `RuntimeWorkerMsg`, runs handle_a2a (or tool invoke) inside `with_scope(msg.scope, ...)`, sends result on `msg.response_tx`.
   - Agent (or registry) holds `Sender<RuntimeWorkerMsg>` to this task; no thread handle, no LocalSet.

3. **Tool session open (BamlRuntimeManager / ToolRegistry)**
   - Add an API that takes scope explicitly, e.g. `open_tool_session(scope, tool_name, open_input)`, and use that from the A2A path and any other caller that has scope from a message.
   - Internally, store `session_id -> scope` (or equivalent) and use it for send/next/finish/abort instead of reading task-local.

4. **Call sites**
   - Where today the code does `with_scope(scope, open_tool_session(...))` and relies on task-local inside open, switch to `open_tool_session(scope, ...)` so scope is passed explicitly and no task-local is required.

This keeps the design MT-friendly, avoids thread-per-agent, and makes all runtime context flow explicit over channels and message payloads.

---

## References: QuickJS runtime worker thread

The [quickjs_runtime](https://hirofa.github.io/quickjs_es_runtime/quickjs_runtime/index.html) crate (and [“Doing something in the runtime worker thread”](https://hirofa.github.io/quickjs_es_runtime/quickjs_runtime/index.html?search=async#doing-something-in-the-runtime-worker-thread)) uses:

- A **single worker-thread EventLoop** for each runtime; all QuickJS API calls are directed there.
- **Thread-safe facades** (e.g. `QuickJsRuntimeFacade`) that you call from any thread; they post jobs to the worker thread.
- **`loop_realm(None, |rt, realm| { ... })`** (or sync variant): the closure runs **on the runtime worker thread**, where you can use the QuickJS adapters.

So the runtime **already has** a worker thread; we do not need to create a separate OS thread per agent. The **runtime worker** in our design can be implemented in either of these ways:

1. **Dedicated thread + block_on (current):** A std thread that runs `block_on(run_runtime_worker(handler, rx))`. Simple and correct; one such thread per “bridge” (per runtime).
2. **Submit jobs to the QuickJS EventLoop:** A task that receives `HandleA2a` and, for each message, submits a fire-and-forget job to the existing QuickJS runtime’s EventLoop (via bridge `post_to_worker_void` backed by `add_task_to_event_loop_void`). The job runs on the **runtime’s** worker thread, drains one queued A2A message, runs `with_scope(scope, handle_a2a(request))`, and sends the result on `response_tx`. No extra thread; one EventLoop per runtime.

Option 2 aligns with the crate’s model (“add a job to the EventLoop”; closure runs in the worker thread) and keeps a single worker thread per runtime. Our internal doc [QUICKJS_THREADING_AND_SCOPE.md](../baml-rt-quickjs/docs/QUICKJS_THREADING_AND_SCOPE.md) describes how scope is passed to native callbacks on that worker thread (thread-local or token → scope map).
