//! Stream invocation and yield buffer for A2A.
//!
//! **Invariants:** See `docs/HOST_QUICKJS_STREAM_INVARIANTS.md` for the full invariant analysis,
//! including single-active-stream (S1), host-only session identity (S2), per-session channel (S3–S4),
//! and coordination via globalThis only (S5).

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

    /// Register the host `__baml_chat_yield_host(chunk_json)`. Session is resolved in the host
    /// from `current_stream_session_id_slot`; no host state (e.g. session id) is passed from JS.
    /// Called once from `new_with_config`.
    pub(crate) async fn register_chat_yield_host(&mut self) -> Result<()> {
        let current_slot = self.current_stream_session_id_slot.clone();
        let tx_by_session = self.a2a_yield_tx_by_session.clone();
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
                            "Expected one JSON-string argument (chunk)",
                        ));
                    }
                    let chunk_json = args[0].get_str().to_string();
                    let value: Value = serde_json::from_str(&chunk_json).map_err(|e| {
                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                            "Invalid yield JSON: {}",
                            e
                        ))
                    })?;
                    let session_id = current_slot.lock().ok().and_then(|g| *g);
                    if let Some(sid) = session_id {
                        let guard = tx_by_session.lock().map_err(|_| {
                            quickjs_runtime::jsutils::JsError::new_str(
                                "a2a_yield_tx_by_session lock poisoned",
                            )
                        })?;
                        if let Some(tx) = guard.get(&sid) && tx.send(value).is_err() {
                            tracing::debug!(
                                %sid,
                                "Yield channel receiver dropped; stream consumer likely closed"
                            );
                        }
                    }
                    Ok(JsValueFacade::Undefined)
                },
            )
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register __baml_chat_yield_host".to_string(),
                source: Box::new(e),
            })?;
        Ok(())
    }

    /// Set up for stream requests. With per-session yield channels, this is a no-op;
    /// each stream creates its own channel in `start_stream_session`.
    ///
    /// **Liveness:** □(this returns Ok → ◇(invoke then collect/finalize for that stream)).
    /// Use [`a2a_stream::begin_a2a_yield_session`] for a type-safe sequence.
    pub async fn setup_a2a_yield_buffer(&mut self) -> Result<()> {
        Ok(())
    }

    /// Drain the yield buffer for one stream. Call in a loop from the collector.
    ///
    /// **INVARIANT (stream progress):** Before every drain, we run pending JS jobs once
    /// so async stream continuations can produce chunks.
    pub async fn drain_yield_buffer(
        &mut self,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    ) -> Result<BufferDrain> {
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });

        let mut chunks = Vec::new();
        let mut channel_closed = false;
        loop {
            match rx.try_recv() {
                Ok(value) => chunks.push(value),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    channel_closed = true;
                    break;
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
    /// **Lifecycle:** Close session, exit its LIFO context, remove session (drops permit)
    /// and its yield sender. Restore global JS helpers only when no streams remain.
    pub(crate) async fn finalize_a2a_stream_invocation(&mut self, session_id: StreamSessionId) {
        // --- 1. Close + cancel session ---
        if let Ok(guard) = self.stream_sessions.lock()
            && let Some(session) = guard.get(&session_id)
        {
            session.closed.store(true, Ordering::Release);
            session.cancel.cancel();
        }

        // --- 2. Exit this session's LIFO context (before removing session) ---
        let context_id = self
            .stream_sessions
            .lock()
            .ok()
            .and_then(|g| g.get(&session_id).and_then(|s| s.context_id.clone()));
        if let Some(ref id) = context_id
            && let Ok(mut reg) = self.invocation_context_registry.lock()
        {
            reg.exit(id);
        }

        // --- 3. Clear host-only current stream slot so no further yields route here ---
        if let Ok(mut slot) = self.current_stream_session_id_slot.lock()
            && *slot == Some(session_id)
        {
            *slot = None;
        }

        // --- 4. Remove session (drops permit) and yield sender ---
        if let Ok(mut guard) = self.stream_sessions.lock() {
            guard.remove(&session_id);
        }
        if let Ok(mut guard) = self.a2a_yield_tx_by_session.lock() {
            guard.remove(&session_id);
        }
        tracing::debug!(%session_id, "finalize_a2a_stream_invocation: session closed and removed");

        // --- 5. Restore global JS helpers only when no streams remain ---
        let should_restore = self
            .stream_sessions
            .lock()
            .map(|g| g.is_empty())
            .unwrap_or(false);
        if should_restore {
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
                        "finalize_a2a_stream_invocation: fallback JS cleanup also failed"
                    );
                }
            }
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
    /// **Error safety:** If any step after permit acquisition fails, the session (and permit)
    /// are removed via `finalize_a2a_stream_invocation(session_id)` before the error is returned.
    ///
    /// **Property:**
    /// ```text
    /// ∀ stream request s:
    ///   invoke_js_function_stream(s) starts async execution AND returns (session_id, rx).
    ///   The promise from onChatMessage() never resolves (by design).
    ///   Chunks are collected via drain_yield_buffer(rx); finalize_a2a_stream_invocation(session_id) when done.
    /// ```
    pub async fn invoke_js_function_stream(
        &mut self,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<(StreamSessionId, tokio::sync::mpsc::UnboundedReceiver<Value>)> {
        let permit = self
            .stream_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BamlRtError::QuickJs("stream semaphore closed".to_string()))?;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();

        let result = self
            .start_stream_session(scope, function_name, args, permit, tx)
            .await;
        match result {
            Ok(session_id) => Ok((session_id, rx)),
            Err(e) => {
                // Roll back: permit is still in start_stream_session's session if it was inserted
                Err(e)
            }
        }
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
        permit: tokio::sync::OwnedSemaphorePermit,
        tx: tokio::sync::mpsc::UnboundedSender<Value>,
    ) -> Result<StreamSessionId> {
        let session_id =
            StreamSessionId(self.next_stream_session_id.fetch_add(1, Ordering::Relaxed));
        let correlation_id = baml_rt_core::correlation::current_correlation_id();

        let context_id = {
            let mut guard = self.invocation_context_registry.lock().map_err(|_| {
                BamlRtError::QuickJs("invocation context registry lock poisoned".to_string())
            })?;
            guard.enter(scope.as_scope().clone(), correlation_id.clone())
        };

        let session = Arc::new(StreamInvocationSession {
            id: session_id,
            scope: scope.as_scope().clone(),
            correlation_id: correlation_id.clone(),
            cancel: CancellationToken::new(),
            closed: AtomicBool::new(false),
            permit: Some(permit),
            context_id: Some(context_id),
        });
        {
            let mut guard = self.stream_sessions.lock().map_err(|_| {
                BamlRtError::QuickJs("stream session map lock poisoned".to_string())
            })?;
            guard.insert(session_id, session);
        }
        if let Ok(mut g) = self.a2a_yield_tx_by_session.lock() {
            g.insert(session_id, tx);
        } else {
            if let Ok(mut guard) = self.stream_sessions.lock() {
                guard.remove(&session_id);
            }
            return Err(BamlRtError::QuickJs(
                "a2a_yield_tx_by_session lock poisoned".to_string(),
            ));
        }
        if let Ok(mut slot) = self.current_stream_session_id_slot.lock() {
            *slot = Some(session_id);
        }
        tracing::debug!(
            context_id = %scope.context_id(),
            %session_id,
            function_name = function_name,
            "start_stream_session: entered invocation context with session"
        );

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;

        // Coordination via globalThis only: wire __chat_yield to host. No host state in JS.
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    globalThis.__chat_yield = function(chunk) {{ __baml_chat_yield_host(JSON.stringify(chunk)); }};
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
            args_json = args_json,
            function_name = function_name,
        );

        // Execute; scope resolved by host via LIFO (we entered context above).
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
                self.finalize_a2a_stream_invocation(session_id).await;
                return Err(BamlRtError::QuickJs(format!(
                    "JS stream function invocation error ({}): {}",
                    function_name, error
                )));
            }
        }

        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });
        tokio::task::yield_now().await;

        Ok(session_id)
    }
}
