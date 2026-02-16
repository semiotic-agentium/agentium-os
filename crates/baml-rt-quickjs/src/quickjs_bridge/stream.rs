use super::QuickJSBridge;
use baml_rt_core::context::InvocationScope;
use baml_rt_core::{BamlRtError, Result};
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::Value;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc::error::TryRecvError;

/// What a single drain of the yield channel observed. Bridge exposes; collector interprets.
#[derive(Debug)]
pub struct BufferDrain {
    pub chunks: Vec<Value>,
    /// True if the sender was dropped (channel closed).
    pub channel_closed: bool,
}

impl QuickJSBridge {
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
    /// **Close semantics:** Waits for all in-flight `__baml_invoke` / `__baml_stream` async
    /// bodies to complete (quiescence barrier), then exits the stream's invocation context,
    /// drops yield channel sender (slot) and receiver (`a2a_yield_rx`), and releases the stream
    /// permit so the next stream may start.
    ///
    /// The quiescence barrier prevents "No invocation context" errors from orphaned promise
    /// continuations that would otherwise fire after the context is torn down.
    pub(crate) async fn finalize_a2a_stream_invocation(&mut self) {
        // --- quiescence barrier ---
        // Drive the event loop until all in-flight async bodies have completed and
        // their resolution tasks have been processed. We require the counter to be 0
        // for two consecutive drain cycles to account for the timing gap between the
        // InFlightGuard drop and the add_task_to_event_loop_void posting.
        const MAX_DRAIN_MS: u64 = 2_000;
        const DRAIN_SLEEP_MS: u64 = 2;
        let start = std::time::Instant::now();
        let mut consecutive_zero = 0u32;

        while start.elapsed().as_millis() < MAX_DRAIN_MS as u128 {
            self.runtime.exe_rt_task_in_event_loop(|rt| {
                rt.run_pending_jobs_if_any();
            });
            tokio::task::yield_now().await;

            if self.in_flight_invoke_count.load(Ordering::Acquire) == 0 {
                consecutive_zero += 1;
                if consecutive_zero >= 2 {
                    break;
                }
            } else {
                consecutive_zero = 0;
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(DRAIN_SLEEP_MS)).await;
        }

        if consecutive_zero < 2 {
            tracing::warn!(
                remaining = self.in_flight_invoke_count.load(Ordering::Acquire),
                elapsed_ms = start.elapsed().as_millis() as u64,
                "finalize_a2a_stream_invocation: quiescence timeout — tearing down context with in-flight promises"
            );
        }

        // --- teardown ---
        if let Some(id) = self.current_stream_context_id.take()
            && let Ok(mut guard) = self.invocation_context_registry.lock()
        {
            guard.exit(&id);
        }
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

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let correlation_id = baml_rt_core::correlation::current_correlation_id();
        let context_id = {
            let mut guard = self.invocation_context_registry.lock().map_err(|_| {
                BamlRtError::QuickJs("invocation context registry lock poisoned".to_string())
            })?;
            guard.enter(scope.as_scope().clone(), correlation_id)
        };
        self.current_stream_context_id = Some(context_id);
        tracing::debug!(
            context_id = %scope.context_id(),
            function_name = function_name,
            "invoke_js_function_stream: entered invocation context (no JS prelude)"
        );

        // No token/context prelude in JS; host resolves scope from active context stack.
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const func = globalThis["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        throw new Error("JS function not found: {}");
                    }}
                    func(args).catch(function(e) {{
                        if (String(e).indexOf('invocation context') >= 0) {{
                            /* expected after stream teardown — orphaned continuation */
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
            args_json, function_name, function_name
        );

        // Execute; native callbacks resolve scope from active context.
        let script = Script::new("invoke_stream.js", &js_code);
        let js_result = self
            .run_eval_with_scope(scope, script, false)
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
        // This ensures the function begins running and can yield chunks
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });

        // Yield to tokio to allow the async function to progress
        tokio::task::yield_now().await;

        Ok(())
    }
}
