use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use baml_rt_core::{BamlRtError, Result, context::InvocationScope};
use quickjs_runtime::{
    jsutils::Script, quickjsrealmadapter::QuickJsRealmAdapter, values::JsValueFacade,
};
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;
use tokio_util::sync::CancellationToken;

use super::{QuickJSBridge, StreamInvocationSession, StreamSessionId, scope::ClearPolicy};

/// What a single drain of the yield channel observed. Bridge exposes; collector interprets.
#[derive(Debug)]
pub struct BufferDrain {
    pub chunks: Vec<Value>,
    /// True if the sender was dropped (channel closed).
    pub channel_closed: bool,
}

impl QuickJSBridge {
    /// Current number of in-flight `__baml_invoke` / `__baml_stream` async bodies.
    pub(crate) fn in_flight_invoke_count(&self) -> u32 {
        self.in_flight_invoke_count.load(Ordering::Acquire)
    }

    /// Set up the chat stream yield buffer and __chat_yield so JS can yield chunks asynchronously
    /// instead of collecting and returning an array. Call before invoking onChatMessage for stream requests.
    ///
    /// **Lifecycle:** Clears any previous yield channel (no stale sender/receiver across sessions).
    /// **Liveness:** □(this returns Ok → ◇(get_a2a_yield_buffer is called after one invoke_js_function("onChatMessage", ·))).
    /// Use [`a2a_stream::begin_a2a_yield_session`] for a type-safe sequence.
    pub async fn setup_a2a_yield_buffer(&mut self) -> Result<()> {
        // Explicit close of previous stream yield channel so no stale state survives.
        self.a2a_yield_rx = None;
        if let Ok(mut slot) = self.a2a_yield_tx_slot.lock() {
            *slot = None;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        {
            let mut slot = self.a2a_yield_tx_slot.lock().map_err(|_| {
                BamlRtError::QuickJs("yield channel slot lock poisoned".to_string())
            })?;
            *slot = Some(tx);
        }
        self.a2a_yield_rx = Some(rx);

        let tx_slot = self.a2a_yield_tx_slot.clone();
        self.runtime
            .set_function(
                &[],
                "__baml_chat_yield_host",
                move |_realm: &QuickJsRealmAdapter,
                      args: Vec<JsValueFacade>|
                      -> std::result::Result<
                    JsValueFacade,
                    quickjs_runtime::jsutils::JsError,
                > {
                    if args.is_empty() || !args[0].is_string() {
                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                            "Expected one JSON-string argument",
                        ));
                    }
                    let chunk_json = args[0].get_str().to_string();
                    let value: Value = serde_json::from_str(&chunk_json).map_err(|e| {
                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                            "Invalid yield JSON: {}",
                            e
                        ))
                    })?;

                    let slot = tx_slot.lock().map_err(|_| {
                        quickjs_runtime::jsutils::JsError::new_str("yield channel slot lock poisoned")
                    })?;
                    if let Some(tx) = slot.as_ref() && tx.send(value).is_err() {
                        tracing::debug!(
                            "Yield channel receiver dropped; stream consumer likely closed"
                        );
                    }
                    Ok(JsValueFacade::Undefined)
                },
            )
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register __baml_chat_yield_host".to_string(),
                source: Box::new(e),
            })?;

        let js_code = r#"
            globalThis.__chat_yield = function(chunk) {
                __baml_chat_yield_host(JSON.stringify(chunk));
            };
        "#;
        let script = Script::new("setup_a2a_yield.js", js_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to set up A2A yield buffer".to_string(),
                source: Box::new(e),
            })?;

        Ok(())
    }

    /// Retrieve and clear the A2A yield buffer contents.
    ///
    /// Returns drained chunks and whether the channel was closed (sender dropped).
    /// Call after invoking a stream function.
    ///
    /// **Liveness:** Requires that `setup_a2a_yield_buffer` was called.
    pub async fn get_a2a_yield_buffer(&mut self) -> Result<BufferDrain> {
        // INVARIANT (stream progress): before every buffer read, drive pending JS jobs once.
        // Without this, async stream continuations may never run, yielding empty polls forever.
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });

        let mut chunks = Vec::new();
        let mut channel_closed = false;
        if let Some(rx) = self.a2a_yield_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(value) => chunks.push(value),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.a2a_yield_rx = None;
                        channel_closed = true;
                        break;
                    }
                }
            }
        }

        Ok(BufferDrain {
            chunks,
            channel_closed,
        })
    }

    /// Finalize a stream invocation after chunk collection completes.
    ///
    /// **Deterministic lifecycle, No quiescence barrier. Instead:
    /// 1. Mark the active session closed and fire its cancellation token.
    /// 2. Remove the session from the map — new callbacks reject immediately.
    /// 3. Restore global JS helpers to non-session natives.
    /// 4. Exit the LIFO invocation context (backward compat).
    /// 5. Drop yield channel + release stream permit.
    ///
    /// In-flight callbacks that already captured an `Arc<StreamInvocationSession>`
    /// can still complete; their `cancel.is_cancelled()` check lets them abort early.
    pub(crate) async fn finalize_a2a_stream_invocation(&mut self) {
        // --- 1. Close + cancel active session ---
        if let Some(session_id) = self.current_stream_session_id.take() {
            if let Ok(guard) = self.stream_sessions.lock()
                && let Some(session) = guard.get(&session_id)
            {
                session.closed.store(true, Ordering::Release);
                session.cancel.cancel();
            }
            // --- 2. Remove from session map ---
            if let Ok(mut guard) = self.stream_sessions.lock() {
                guard.remove(&session_id);
            }
            tracing::debug!(%session_id, "finalize_a2a_stream_invocation: session closed and removed");
        }

        // --- 3. Restore global JS helpers to non-session natives ---
        // This ensures non-stream evaluate() calls after this stream use the LIFO registry.
        let restore_code = r#"
            (function() {
                if (typeof __orig_baml_invoke !== 'undefined') {
                    globalThis.__baml_invoke = __orig_baml_invoke;
                    globalThis.__baml_stream = __orig_baml_stream;
                    globalThis.__tool_invoke = __orig_tool_invoke;
                    globalThis.__tool_from_baml_result = __orig_tool_from_baml_result;
                    globalThis.__tool_session_open = __orig_tool_session_open;
                    globalThis.__tool_session_send = __orig_tool_session_send;
                    globalThis.__tool_session_next = __orig_tool_session_next;
                    globalThis.__tool_session_finish = __orig_tool_session_finish;
                    globalThis.__tool_session_abort = __orig_tool_session_abort;
                    delete globalThis.__orig_baml_invoke;
                    delete globalThis.__orig_baml_stream;
                    delete globalThis.__orig_tool_invoke;
                    delete globalThis.__orig_tool_from_baml_result;
                    delete globalThis.__orig_tool_session_open;
                    delete globalThis.__orig_tool_session_send;
                    delete globalThis.__orig_tool_session_next;
                    delete globalThis.__orig_tool_session_finish;
                    delete globalThis.__orig_tool_session_abort;
                }
            })()
        "#;
        let script = Script::new("restore_globals.js", restore_code);
        if let Err(e) = self.runtime.eval(None, script).await {
            tracing::error!(
                error = ?e,
                "finalize_a2a_stream_invocation: primary JS global restore failed, \
                 attempting minimal fallback"
            );
            // Fallback: delete stale __orig_* markers so the next stream IIFE does not
            // save already-overridden wrappers as "originals". The globals remain pointing
            // at session-aware wrappers whose session was already removed — subsequent
            // calls through them will fail with "session not found" rather than silently
            // routing to a stale scope. This is the best we can do when the JS runtime
            // is in a degraded state.
            let fallback = r#"
                (function() {
                    try {
                        delete globalThis.__orig_baml_invoke;
                        delete globalThis.__orig_baml_stream;
                        delete globalThis.__orig_tool_invoke;
                        delete globalThis.__orig_tool_from_baml_result;
                        delete globalThis.__orig_tool_session_open;
                        delete globalThis.__orig_tool_session_send;
                        delete globalThis.__orig_tool_session_next;
                        delete globalThis.__orig_tool_session_finish;
                        delete globalThis.__orig_tool_session_abort;
                    } catch(e) { /* best effort */ }
                })()
            "#;
            let fallback_script = Script::new("restore_globals_fallback.js", fallback);
            if let Err(e2) = self.runtime.eval(None, fallback_script).await {
                tracing::error!(
                    error = ?e2,
                    "finalize_a2a_stream_invocation: fallback JS cleanup also failed — \
                     bridge JS globals are in an unrecoverable state; subsequent \
                     stream invocations may fail with 'session not found'"
                );
            }
        }

        // --- 4. Exit LIFO context (backward compat) ---
        if let Some(id) = self.current_stream_context_id.take()
            && let Ok(mut guard) = self.invocation_context_registry.lock()
        {
            guard.exit(&id);
        }

        // --- 5. Teardown channels + permit ---
        self.current_stream_token = None;
        self.stream_permit = None;
        self.a2a_yield_rx = None;
        if let Ok(mut slot) = self.a2a_yield_tx_slot.lock() {
            *slot = None;
        }
    }

    /// Invoke a JavaScript function for streaming (yield-based) requests.
    ///
    /// **Scope / conversation routing:** Caller must pass the invocation scope for this request
    /// (one scope per A2A conversation). The entire JS run executes inside that scope so yielded
    /// chunks are attributed to the correct conversation. Multiple parallel conversations each
    /// use their own scope when the host invokes this (per request).
    ///
    /// **INVARIANT L6 (Stream Promise Non-Termination):**
    /// For stream requests, the promise from `onChatMessage()` is DESIGNED to never resolve.
    /// It yields chunks via `__chat_yield()` and only completes on agent exit or crash.
    /// This method starts the async function but does NOT wait for promise resolution.
    ///
    /// **Error safety:** If any step after permit acquisition fails, all partially-installed
    /// state (session map entry, LIFO context, JS global overrides, permit) is cleaned up
    /// via `finalize_a2a_stream_invocation` before the error is returned.
    ///
    /// **Cancellation recovery (lazy):** If a previous invocation future was dropped or
    /// cancelled mid-await, `stream_permit` remains `Some` and the semaphore has zero
    /// available permits. Without recovery, the next `acquire_owned().await` would deadlock.
    /// We detect this at entry and finalize the stale state before re-acquiring.
    ///
    /// In practice this is extremely unlikely: the only production call site
    /// (`QuickJsInvoker::invoke_stream` in `request_router.rs`) wraps the entire
    /// begin→invoke→collect chain inside `spawn_blocking` + `block_on`, which is not
    /// cancellable via `tokio::select!`. The guard exists as defense-in-depth in case
    /// future call sites do not provide the same guarantee.
    ///
    /// **Property:**
    /// ```text
    /// ∀ stream request s:
    ///   invoke_js_function_stream(s) starts async execution AND returns immediately
    ///   The promise from onChatMessage() never resolves (by design)
    ///   Chunks are collected via get_a2a_yield_buffer() after invocation
    /// ```
    pub async fn invoke_js_function_stream(
        &mut self,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<()> {
        // Lazy recovery: if a previous invocation future was dropped/cancelled after
        // acquiring the permit but before finalization ran, the permit and partial
        // session state are still held. Finalize first so the semaphore is freed and
        // the next acquire does not deadlock.
        if self.stream_permit.is_some() {
            tracing::warn!(
                "invoke_js_function_stream: recovering stale stream state \
                 from a previously dropped/cancelled future"
            );
            self.finalize_a2a_stream_invocation().await;
        }

        // Only one stream active at a time so invocation token state is not overwritten by a concurrent stream.
        let permit = self
            .stream_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BamlRtError::QuickJs("stream semaphore closed".to_string()))?;
        self.stream_permit = Some(permit);

        // Exit previous stream's context so the next stream has a clean stack.
        if let Some(prev_id) = self.current_stream_context_id.take() {
            if let Ok(mut guard) = self.invocation_context_registry.lock() {
                guard.exit(&prev_id);
            }
            tracing::debug!("invoke_js_function_stream: exited previous stream context");
        }
        if let Some(prev) = self.current_stream_token.take() {
            self.remove_invocation_token(&prev);
        }

        // All remaining work is fallible. On error, finalize cleans up any
        // partially-installed state (session, LIFO context, globals, permit).
        let result = self.start_stream_session(scope, function_name, args).await;
        if result.is_err() {
            self.finalize_a2a_stream_invocation().await;
        }
        result
    }

    /// Fallible body of [`invoke_js_function_stream`]: allocates the session, enters
    /// the LIFO context, overrides JS globals, and kicks off the stream function.
    ///
    /// On success the stream is running and the caller collects chunks via
    /// `get_a2a_yield_buffer`. On error the caller must call
    /// `finalize_a2a_stream_invocation` to roll back partial state.
    async fn start_stream_session(
        &mut self,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<()> {
        // --- Allocate session ---
        let session_id =
            StreamSessionId(self.next_stream_session_id.fetch_add(1, Ordering::Relaxed));
        let correlation_id = baml_rt_core::correlation::current_correlation_id();
        let session = Arc::new(StreamInvocationSession {
            id: session_id,
            scope: scope.as_scope().clone(),
            correlation_id: correlation_id.clone(),
            cancel: CancellationToken::new(),
            closed: AtomicBool::new(false),
        });
        {
            let mut guard = self.stream_sessions.lock().map_err(|_| {
                BamlRtError::QuickJs("stream session map lock poisoned".to_string())
            })?;
            guard.insert(session_id, session);
        }
        self.current_stream_session_id = Some(session_id);

        // Push to LIFO registry for backward compat (non-session natives still use it).
        let context_id = {
            let mut guard = self.invocation_context_registry.lock().map_err(|_| {
                BamlRtError::QuickJs("invocation context registry lock poisoned".to_string())
            })?;
            guard.enter(scope.as_scope().clone(), correlation_id)
        };
        self.current_stream_context_id = Some(context_id);
        tracing::debug!(
            context_id = %scope.context_id(),
            %session_id,
            function_name = function_name,
            "start_stream_session: entered invocation context with session"
        );

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let sid = session_id.0;

        // Generate JS IIFE that:
        // 1. Saves original global helpers
        // 2. Overrides them with session-aware versions (session_id baked in)
        // 3. Calls the stream function
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    // Save originals so finalization can restore them
                    globalThis.__orig_baml_invoke = globalThis.__baml_invoke;
                    globalThis.__orig_baml_stream = globalThis.__baml_stream;
                    globalThis.__orig_tool_invoke = globalThis.__tool_invoke;
                    globalThis.__orig_tool_from_baml_result = globalThis.__tool_from_baml_result;
                    globalThis.__orig_tool_session_open = globalThis.__tool_session_open;
                    globalThis.__orig_tool_session_send = globalThis.__tool_session_send;
                    globalThis.__orig_tool_session_next = globalThis.__tool_session_next;
                    globalThis.__orig_tool_session_finish = globalThis.__tool_session_finish;
                    globalThis.__orig_tool_session_abort = globalThis.__tool_session_abort;

                    // Override with session-aware versions
                    var __sid = {sid};
                    globalThis.__baml_invoke = function(a, b) {{ return __baml_invoke_session(__sid, a, b); }};
                    globalThis.__baml_stream = function(a, b) {{ return __baml_stream_session(__sid, a, b); }};
                    globalThis.__tool_invoke = function(a, b) {{ return __tool_invoke_session(__sid, a, b); }};
                    globalThis.__tool_from_baml_result = function(a) {{ return __tool_from_baml_result_session(__sid, a); }};
                    globalThis.__tool_session_open = function(a, b) {{ return __tool_session_open_session(__sid, a, b); }};
                    globalThis.__tool_session_send = function(a, b) {{ return __tool_session_send_session(__sid, a, b); }};
                    globalThis.__tool_session_next = function(a) {{ return __tool_session_next_session(__sid, a); }};
                    globalThis.__tool_session_finish = function(a) {{ return __tool_session_finish_session(__sid, a); }};
                    globalThis.__tool_session_abort = function(a, b) {{ return __tool_session_abort_session(__sid, a, b); }};

                    const args = {args_json};
                    const func = globalThis["{function_name}"];
                    if (func === undefined || typeof func !== 'function') {{
                        throw new Error("JS function not found: {function_name}");
                    }}
                    func(args).catch(function(e) {{
                        var msg = String(e);
                        if (msg.indexOf('invocation context') >= 0 ||
                            msg.indexOf('cancelled') >= 0 ||
                            msg.indexOf('not found') >= 0) {{
                            /* expected after stream teardown or cancellation */
                        }} else {{
                            throw e;
                        }}
                    }});
                    return JSON.stringify({{ success: true }});
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            sid = sid,
            args_json = args_json,
            function_name = function_name,
        );

        // Execute; session-aware natives resolve scope from session map.
        let script = Script::new("invoke_stream.js", &js_code);
        let js_result = self
            .run_eval_with_scope(scope, script, ClearPolicy::Keep)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: format!("Failed to invoke stream function {}", function_name),
                source: Box::new(e),
            })?;

        // Check for immediate errors (synchronous errors, function not found, etc.)
        if js_result.is_string() {
            let json_str = js_result.get_str();
            if let Ok(value) = serde_json::from_str::<Value>(json_str)
                && let Some(error) = value.get("error").and_then(Value::as_str)
            {
                return Err(BamlRtError::QuickJs(format!(
                    "JS stream function invocation error ({}): {}",
                    function_name, error
                )));
            }
        }

        // Run pending jobs to allow the async function to start executing
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });

        // Yield to tokio to allow the async function to progress
        tokio::task::yield_now().await;

        Ok(())
    }
}
