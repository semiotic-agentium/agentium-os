# A2A Session Channel Components: Invariants and Liveness

This document describes the invariant and liveness properties of the channel-based A2A session dispatcher and runtime worker. Property tests encode these and live in `tests/` with names `prop_*`.

---

## 1. Session Dispatcher

The dispatcher is a single tokio task that receives `DispatcherMsg` and maintains a map `session_id → (scope, response_tx)`. It forwards `HandleA2a` messages to the runtime worker.

### 1.1 Session uniqueness (invariant)

**Property:**

```
∀ session_id: EXISTS at most one entry in the map WHERE key = session_id
```

At any time, the dispatcher map contains at most one `(scope, response_tx)` per `session_id`. A second `Register` for the same `session_id` before `Finish`/`Abort` would overwrite (or we reject); we define that `Register` is sent exactly once per session at open.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Single channel ordering: for a given session, `Register` is sent once at open, then only `Cmd` messages. No duplicate `Register(session_id, ...)` from the same session. |
| **Testing**     | `prop_dispatcher_session_at_most_once` (after N messages, each session_id appears at most once in the map). |

### 1.2 Register-before-Cmd ordering (invariant)

**Property:**

```
∀ session_id, ∀ Cmd(session_id, Send(_)):
  ∃ a prior message Register(session_id, scope, response_tx) in the same channel order
```

For every `Cmd(session_id, Send(request))`, the dispatcher has already processed a `Register(session_id, scope, response_tx)` for that `session_id`. So the map has an entry when we process Send.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Single ordered channel: caller sends `Register` at open, then only `Cmd`. So message order guarantees Register before Cmd for that session. |
| **Testing**     | `prop_dispatcher_register_before_cmd`: generate sequences of messages; for any Cmd(Send) the corresponding Register must have been applied first. |

### 1.3 Finish/Abort removes session (invariant)

**Property:**

```
∀ session_id: AFTER processing Cmd(session_id, Finish) or Cmd(session_id, Abort(_)):
  session_id ∉ map
```

After processing Finish or Abort for a session, that session is removed from the map and the response_tx is dropped (so the receiver gets closed).

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Dispatcher matches `Cmd(id, Finish)` and `Cmd(id, Abort(_))` and calls `map.remove(id)`. |
| **Testing**     | `prop_dispatcher_finish_removes_session`: apply Register, then Finish (or Abort), assert map no longer contains session_id. |

### 1.4 Context in messages (invariant)

**Property:**

```
∀ Register(session_id, scope, response_tx): scope is carried in the message (no task-local)
∀ Cmd(session_id, Send(request)): dispatcher looks up scope from map(session_id) and forwards scope in HandleA2a
```

All runtime context (scope) is carried in channel messages or stored in the map from Register; the dispatcher never reads task-local for scope.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Register carries scope; map stores (scope, response_tx); HandleA2a is built from map lookup. No use of `task_local_context()` in dispatcher. |
| **Testing**     | Code review; unit test that dispatcher output HandleA2a has scope equal to the one from Register. |

---

## 2. Runtime Worker

The runtime worker is a tokio task that receives `RuntimeWorkerMsg` (e.g. `HandleA2a { scope, request, response_tx }`), calls `run_handle_a2a(scope, request)`, and sends results on `response_tx`. The handler implementation enqueues A2A work and posts a fire-and-forget worker drain via `post_to_worker_void`.

### 2.1 Scope from message (invariant)

**Property:**

```
∀ HandleA2a { scope, request, response_tx }: the handler runs inside with_scope(scope, ...); scope is not read from task-local
```

The worker never uses task-local to obtain scope; it uses only the scope carried in the message.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Worker code: `context::with_scope(msg.scope, handler.handle_a2a(msg.request)).await`; no task-local scope lookup in worker. |
| **Testing**     | Unit test with mock handler that asserts it was called with the same scope as in the message (e.g. scope stored in handler mock). |

### 2.2 One response per HandleA2a (invariant)

**Property:**

```
∀ HandleA2a { scope, request, response_tx }: AFTER the worker processes the message,
  exactly one send of Vec<Value> (or error) occurs on response_tx (multiple values for stream, then channel unchanged or closed by sender)
```

The worker sends all response values (or one error value) on the provided `response_tx`; it does not drop the message without sending.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Worker match on HandleA2a, run handle_a2a, then for each value in the result `let _ = response_tx.send(v)`. On error, send one error value. |
| **Testing**     | `prop_worker_sends_response`: mock handler returns known Vec<Value>; assert receiver gets the same values. |

### 2.3 Liveness: eventual response (liveness)

**Property:**

```
IF dispatcher sends HandleA2a { scope, request, response_tx } to the worker channel,
THEN eventually the worker sends at least one value (or error) on response_tx
```

Under fair scheduling, every HandleA2a message sent to the worker is eventually processed and the worker sends the result on `response_tx`. (We assume the worker loop is running and the handler completes.)

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Worker loop is a single-threaded loop; no dropping of messages. Handler is awaited to completion. |
| **Testing**     | `prop_worker_eventual_response`: send HandleA2a, await with timeout on response_rx; assert we get at least one value or channel closed after error. |

---

## 3. End-to-end session (invariants)

### 3.1 Send-after-finish rejected (invariant)

**Property:**

```
∀ session_id: IF Finish(session_id) or Abort(session_id) has been processed,
THEN a subsequent Send(session_id, request) from the session API is rejected (session FSM in Closed state)
```

The session FSM (host-side) rejects send() after finish() or abort(). So the client cannot send after close.

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | A2aSession tracks phase; send() returns Err if phase is Closing or Closed. |
| **Testing**     | `test_a2a_session_send_after_finish_fails` (already present). |

### 3.2 Response ordering per session (invariant)

**Property:**

```
∀ session_id: values received on response_rx for that session are in the same order as the HandleA2a requests processed for that session (per-session FIFO)
```

For a single session, responses are delivered in the order of the Send commands (and the worker processes one HandleA2a at a time per runtime, so ordering is preserved).

**Enforcement:**

| Layer           | Mechanism |
|----------------|-----------|
| **Application** | Single channel to worker; worker processes one message at a time. So responses for the same session are sent in request order. |
| **Testing**     | Property test: multiple Send for one session; collect responses; assert order matches request order (with mock handler). |

### 3.3 Explicit completion drives continuation (invariant)

**Property:**

```
∀ streamed turn t:
  continuation(t) = f(completion(t))
  where completion(t) ∈ {SemanticFinal, InputRequired, ChannelClosed, Timeout}
```

The live stream session does not infer continuation from chunk JSON shape. It forwards
all formatted chunks and then transitions strictly from `StreamResult.completion`:

- `InputRequired` → wait for exactly one next input turn
- `SemanticFinal | ChannelClosed | Timeout` → close session

This removes the prior ambiguous branch (`¬final ∧ ¬input_required`) that could either
hang or be prematurely closed depending on heuristics.

**Enforcement:**

| Layer | Mechanism |
|-------|-----------|
| **Collection** | `A2aYieldSessionComplete::collect()` returns explicit `StreamCompletion` (`SemanticFinal`, `InputRequired`, `ChannelClosed`, `Timeout`). |
| **Transport** | `run_live_stream_session` matches on `stream_result.completion`; no chunk-shape booleans for control flow. |
| **Testing** | `a2a` unit tests cover stream and task paths; hangs regress to watchdog timeout failures. |

---

## 4. Property test index

| Test name | Component | Property |
|-----------|-----------|----------|
| `prop_dispatcher_session_at_most_once` | Dispatcher | Session uniqueness |
| `prop_dispatcher_register_before_cmd` | Dispatcher | Register-before-Cmd ordering |
| `prop_dispatcher_finish_removes_session` | Dispatcher | Finish/Abort removes session |
| `prop_worker_sends_response` | Worker | One response per HandleA2a |
| `prop_worker_eventual_response` | Worker | Liveness: eventual response |
| `prop_worker_eventual_response_with_message_scope` | Worker | Scope from message + eventual response |
| `prop_dispatcher_forwards_registered_scope_and_payload` | Dispatcher | Forwarding preserves registered scope + payload order |
| `prop_dispatcher_finish_removes_session_and_blocks_further_sends` | Dispatcher | Finish removes route and blocks future sends |
| `prop_interleaved_a2a_tool_llm_multi_context_isolation` | A2A + runtime | Concurrent multi-context isolation under jittered A2A/tool/LLM interleavings |
| `prop_input_required_resume_positive_and_no_auto_final` | A2A + runtime | InputRequired two-turn invariant: no auto-final on ask, deterministic final on same-context resume |
