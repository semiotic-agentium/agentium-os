//! Stream invocation and yield buffer for A2A.
//!
//! **Invariants:** See `README.md` in this crate for the canonical stream architecture and
//! host-authoritative routing invariants.

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

use super::{
    QuickJSBridge, StreamInvocationSession, StreamSessionId,
    scope::{ClearPolicy, resolve_scope_from_active_context},
};

/// What a single drain of the yield channel observed. Bridge exposes; collector interprets.
#[derive(Debug)]
pub struct BufferDrain {
    pub chunks: Vec<Value>,
    /// True if the sender was dropped (channel closed).
    pub channel_closed: bool,
}

impl QuickJSBridge {
    /// Current number of in-flight `__baml_invoke` / `__baml_stream` async bodies.
    /// Reserved for diagnostics and future effect-gated behaviour (e.g. optional backpressure).
    #[allow(dead_code)] // reserved for diagnostics; not yet used in production path
    pub(crate) fn in_flight_invoke_count(&self) -> u32 {
        self.in_flight_invoke_count.load(Ordering::Acquire)
    }

    /// Register the host `__baml_chat_yield_host(chunk_json)`.
    /// Session is resolved from the active QuickJS invocation context.
    /// Called once from `new_with_config`.
    pub(crate) async fn register_chat_yield_host(&mut self) -> Result<()> {
        let tx_by_session = self.a2a_yield_tx_by_session.clone();
        let stream_sessions = self.stream_sessions.clone();
        let invocation_context_registry = self.invocation_context_registry.clone();
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
                    let mut value: Value = serde_json::from_str(&chunk_json).map_err(|e| {
                        quickjs_runtime::jsutils::JsError::new_str(&format!("Invalid yield JSON: {}", e))
                    })?;

                    // Resolve routing from host-owned state only: explicit __session tag first,
                    // then active invocation scope + stream session map.
                    let context_session_id = value
                        .get("__session")
                        .and_then(|v| v.as_u64())
                        .map(StreamSessionId)
                        .or_else(|| {
                            resolve_scope_from_active_context(&invocation_context_registry)
                                .ok()
                                .and_then(|scope| {
                                    stream_sessions.lock().ok().and_then(|guard| {
                                        guard.iter().find_map(|(sid, session)| {
                                            (!session.is_terminated() && session.scope == scope)
                                                .then_some(*sid)
                                        })
                                    })
                                })
                        });

                    if value.get("__session").is_some()
                        && let Some(obj) = value.as_object_mut()
                    {
                        obj.remove("__session");
                    }

                    tracing::trace!(chunk_json = %chunk_json, "yield raw");

                    if let Some(sid) = context_session_id {
                        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
                            eprintln!("stream_yield_route: routed session={sid}");
                        }
                        let guard = tx_by_session.lock().map_err(|_| {
                            quickjs_runtime::jsutils::JsError::new_str(
                                "a2a_yield_tx_by_session lock poisoned",
                            )
                        })?;
                        if let Some(tx) = guard.get(&sid) {
                            if tx.send(value).is_err() {
                                tracing::debug!(
                                    %sid,
                                    "Yield channel receiver dropped; stream consumer likely closed"
                                );
                            }
                        } else {
                            tracing::warn!(
                                %sid,
                                "yield host: session not found in active route map"
                            );
                        }
                    } else {
                        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
                            let scope = resolve_scope_from_active_context(&invocation_context_registry)
                                .ok()
                                .map(|scope| format!("{:?}", scope));
                            let in_flight_sessions = stream_sessions
                                .lock()
                                .ok()
                                .map(|guard| guard.len())
                                .unwrap_or(0);
                            eprintln!(
                                "stream_yield_route: no active session; scope={scope:?}; in_flight={in_flight_sessions}; chunk={chunk_json}"
                            );
                        }
                        tracing::warn!(
                            chunk_json = %chunk_json,
                            "yield host: no active session for routing"
                        );
                    }
                    Ok(JsValueFacade::Undefined)
                },
            )
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register __baml_chat_yield_host".to_string(),
                source: Box::new(e),
            })?;

        let install_code = Script::new(
            "install_chat_yield.js",
            r#"(function() {
                globalThis.__chat_yield = function(chunk) {
                    __baml_chat_yield_host(JSON.stringify(chunk));
                };
            })()"#,
        );
        self.runtime.eval(None, install_code).await.map_err(|e| {
            BamlRtError::QuickJsWithSource {
                context: "Failed to install __chat_yield bridge".to_string(),
                source: Box::new(e),
            }
        })?;
        Ok(())
    }

    /// Set up for stream requests. With per-session yield channels this is a no-op;
    /// each stream creates its own channel in the session. Used by [`crate::a2a_stream::begin_a2a_yield_session`].
    pub async fn setup_a2a_yield_buffer(&mut self) -> Result<()> {
        Ok(())
    }

    /// Drain the yield buffer for one stream. Call in a loop from the collector.
    ///
    /// **INVARIANT (stream progress):** Before every drain we run pending JS jobs once so
    /// async stream continuations can produce chunks (align with main).
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

    /// Advance QuickJS pending work without draining the yield receiver.
    ///
    /// Keep this separated from [`drain_yield_buffer`] to allow callers to control lock scope.
    /// Reserved for future use when a caller needs to advance without draining.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn advance_pending_jobs(&self) {
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });
    }

    /// Finalize a stream invocation after chunk collection completes.
    ///
    /// **Lifecycle:** Close session, teardown tool sessions for this context, exit its LIFO
    /// context, remove session (drops permit) and its yield sender. Restore global JS helpers
    /// only when no streams remain.
    pub(crate) async fn finalize_a2a_stream_invocation(&mut self, session_id: StreamSessionId) {
        // --- 1. Resolve runtime context_id for teardown (read-only; don't close session yet) ---
        let invocation_context_id = self
            .stream_sessions
            .lock()
            .ok()
            .and_then(|g| g.get(&session_id).and_then(|s| s.context_id.clone()));
        let runtime_context_id = invocation_context_id.as_ref().and_then(|id| {
            self.invocation_context_registry
                .lock()
                .ok()
                .and_then(|reg| reg.get_context_id(id))
        });

        // --- 2. Drain event-loop pending jobs so any __baml_invoke (and other) continuations
        // run while the context and session are still valid. Context is non-nullable; we must
        // not close or exit until the runtime has processed queued work for this stream.
        // Run on current thread; runtime is not necessarily Arc in all builds.
        self.runtime
            .exe_rt_task_in_event_loop(|r| r.run_pending_jobs_if_any());

        // --- 3. Close + cancel session (only after drain so natives see valid session) ---
        if let Ok(guard) = self.stream_sessions.lock()
            && let Some(session) = guard.get(&session_id)
        {
            session.closed.store(true, Ordering::Release);
            session.cancel.cancel();
        }

        // --- 4. Close all tool sessions for this context (deterministic teardown, no leak) ---
        if let Some(ref cid) = runtime_context_id {
            let mgr = self.baml_manager.lock().await;
            if let Err(e) = mgr.close_sessions_for_context(cid).await {
                tracing::warn!(
                    error = %e,
                    %session_id,
                    context_id = %cid,
                    "finalize_a2a_stream_invocation: close_sessions_for_context failed, continuing teardown"
                );
            }
        }

        // --- 5. Exit this session's LIFO context (before removing session) ---
        if let Some(ref id) = invocation_context_id
            && let Ok(mut reg) = self.invocation_context_registry.lock()
        {
            reg.exit(id);
        }

        // --- 6. Remove session (drops permit) and yield sender ---
        if let Ok(mut guard) = self.stream_sessions.lock() {
            guard.remove(&session_id);
        }
        if let Ok(mut guard) = self.a2a_yield_tx_by_session.lock() {
            guard.remove(&session_id);
        }
        tracing::debug!(%session_id, "finalize_a2a_stream_invocation: session closed and removed");

        // Do NOT restore or remove global JS helpers (__baml_invoke, __baml_stream, etc.).
        // Multiple agents share the same QuickJS engine; globals are process-wide and must
        // not be touched when one stream finalizes.
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
    /// On success the stream is running and the caller drains yielded chunks via
    /// `collect_into_channel_owned`. On error the caller must call
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

        // Pre-build conversation context tags for resume (stream path). Hold lock only for the async call.
        let context_tags = self
            .baml_manager
            .lock()
            .await
            .build_conversation_context_tags(scope.as_scope())
            .await
            .ok()
            .flatten();

        let session = Arc::new(StreamInvocationSession {
            id: session_id,
            scope: scope.as_scope().clone(),
            correlation_id: correlation_id.clone(),
            cancel: CancellationToken::new(),
            closed: AtomicBool::new(false),
            permit: Some(permit),
            context_id: Some(context_id),
            context_tags,
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
        tracing::debug!(
            context_id = %scope.context_id(),
            %session_id,
            function_name = function_name,
            "start_stream_session: entered invocation context with session"
        );

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let session_id_num = session_id.0;

        let js_code = format!(
            r#"
            (function() {{
                const sid = {session_id_num};
                try {{
                    const args = {args_json};
                    if (Array.isArray(args) && args[0] && typeof args[0] === 'object') {{
                        args[0] = Object.assign({{}}, args[0], {{ __session: sid }});
                    }} else if (args && typeof args === 'object' && !Array.isArray(args)) {{
                        Object.assign(args, {{ __session: sid }});
                    }}
                    const func = globalThis["{function_name}"];
                    if (func === undefined || typeof func !== 'function') {{
                        throw new Error("JS function not found: {function_name}");
                    }}
                    function wrapSession(p) {{
                        if (!p || typeof p.then !== 'function') return p;
                        return p.then(
                            function(v) {{ return wrapSession(v); }},
                            function(e) {{ throw e; }}
                        );
                    }}
                    const p = func(args).catch(function(e) {{
                        var msg = String(e);
                        if (msg.indexOf('invocation context') >= 0 ||
                            msg.indexOf('cancelled') >= 0 ||
                            msg.indexOf('not found') >= 0) {{
                        }} else {{
                            throw e;
                        }}
                    }});
                    return wrapSession(p).then(
                        function() {{ return JSON.stringify({{ success: true }}); }},
                        function(e) {{ return JSON.stringify({{ error: e.message || String(e) }}); }}
                    );
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            session_id_num = session_id_num,
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
