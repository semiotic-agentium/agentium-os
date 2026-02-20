# Argument-sketch E2E stream trace: why Chapman’s reply is missing

## Summary

**Symptom:** `test_e2e_argument_sketch_two_agents` expects at least two message chunks (Cleese then Chapman) but only sees Cleese’s “No I didn’t.”

**Root cause:** `A2aYieldSessionComplete::collect()` returns on the **first** non-empty buffer read. Cleese yields one chunk (`ctx.emit.message(myLine)`); collect() returns immediately and never drives the JS event loop again, so the agent never reaches `await CleeseSendToChapman` and `emitFromToolResult`. Chapman's reply is never yielded. (CleeseSendToChapman is **non-streaming** (__baml_invoke); the executor returns `Value::Array(chunks)` to JS correctly, but we never get that far.)

## Flow

1. **Test** calls `cleese_agent.handle_a2a(send_message_request("Start the argument."))`.
2. **Transport** turns that into a stream request and calls `router.route` → `js_invoker.invoke_stream` → bridge `begin_a2a_yield_session().invoke(scope, js_request).collect()`.
3. **Cleese agent** runs `run(ctx)`:
   - `await ArgumentReply({ other_message: text })` → Cleese’s line (“No I didn’t.”).
   - `ctx.emit.message(myLine)` → host `__chat_yield` → **first chunk** (Cleese) is collected ✓
   - `await CleeseSendToChapman({ first_line: myLine })` → calls **__baml_stream** (or __baml_invoke) for that BAML function.
4. **__baml_stream** (quickjs_bridge.rs):
   - Builds `stream` via `manager.invoke_function_stream(...)`.
   - Spawns a task that runs `stream.run(..., Some(|result| { if let Some(Ok(parsed)) = result.parsed() { tx.try_send(parsed_value) } }), ...)`.
   - **Only results with `result.parsed()` are sent** to `tx`. So we send:
     - The **session plan** (Open/Send/Next/Finish) when the LLM returns it ✓
   - The BAML runtime then calls **our executor** to run that plan (same process, same task).
5. **Executor** (baml-rt-quickjs/baml.rs `execute_tool_session_plan`):
   - Opens `system/internal_a2a` session, sends “No I didn’t.” to Chapman.
   - **A2aSession** (tools/system/a2a_session.rs): `send()` spawns a task that calls `handler.handle_a2a_stream(request)` (Chapman’s stream), forwards each response into a channel; `next()` recv’s from that channel and returns `ToolStep::Streaming { output }` for each Chapman chunk.
   - Executor collects all `ToolStep::Streaming` into `streaming_outputs`, then returns **`Ok(Value::Array(streaming_outputs))`** (array of A2A chunk objects).
6. **BAML runtime** receives that array from the executor. It does **not** invoke our stream callback with that value (the callback is only driven by `result.parsed()` for LLM turns). So **no Chapman chunks are pushed to `tx`**.
7. **__baml_stream** collects from `rx`: we get `[ plan ]` only. We return that array to JS.
8. **Agent** does `emitFromToolResult(ctx.emit, chapmanResult)` with `chapmanResult = [ plan ]`. `emitFromToolResult` iterates; the plan has no `message.parts[0].text`, so **nothing is emitted for Chapman** ✓ (explains the failure).

## Fix (implemented)

Emit tool-session streaming outputs into the same stream channel when we are the executor and we return `Value::Array(streaming_outputs)` from a session plan. That way the JS receives `[ plan, chunk1, chunk2, ... ]` and `emitFromToolResult` can emit each Chapman chunk.

- **Mechanism:** Task-local optional “stream yield” sender. In `__baml_stream` we set it to `Some(tx)` before `stream.run`, and in `execute_tool_session_plan` when we return `Ok(Value::Array(streaming_outputs))` we, if the task-local sender is set, send each element to it before returning.
- **Result:** The stream channel gets the plan (from the callback) and then each Chapman chunk (from the executor), so the agent sees both and the test sees two message chunks.
