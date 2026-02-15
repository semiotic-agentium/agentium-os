use super::QuickJSBridge;
use crate::quickjs_bridge::eval::drive_event_loop;
use baml_rt_core::context::InvocationScope;
use baml_rt_core::{BamlRtError, Result};
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;

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

    /// Retrieve and clear the A2A yield buffer contents
    ///
    /// This should be called after invoking a stream function.
    ///
    /// **Liveness:** Requires that `setup_a2a_yield_buffer` was called.
    pub async fn get_a2a_yield_buffer(&mut self) -> Result<Vec<Value>> {
        // INVARIANT (stream progress): before every buffer read, drive the QuickJS event loop
        // so that externally-resolved promises (from JsValueFacade::new_promise tokio tasks)
        // are picked up and their JS continuations executed. Using eval() processes the full
        // event loop task queue, not just internal microtasks, ensuring BAML function results
        // that resolved on a tokio thread are delivered back to JS.
        if let Err(err) = drive_event_loop(&self.runtime).await {
            tracing::warn!(error = ?err, "QuickJS drive_event_loop failed");
        }

        let mut responses = Vec::new();
        if let Some(rx) = self.a2a_yield_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(value) => responses.push(value),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.a2a_yield_rx = None;
                        break;
                    }
                }
            }
        }

        Ok(responses)
    }

    /// Finalize a stream invocation after chunk collection completes.
    ///
    /// **Close semantics:** Drops yield channel sender (slot) and receiver (a2a_yield_rx) so no
    /// stale stream state survives; next stream gets a fresh channel from setup_a2a_yield_buffer.
    /// Also releases stream permit so the next stream may start.
    pub(crate) fn finalize_a2a_stream_invocation(&mut self) {
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

        // Remove previous stream's token so it cannot be reused; leave current token for late continuations until next stream.
        if let Some(prev) = self.current_stream_token.take() {
            self.remove_invocation_token(&prev);
            tracing::debug!(token = %prev.0, "invoke_js_function_stream: removed previous stream token");
        }

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let (token, token_prelude) = self.create_invocation_token(scope);
        self.current_stream_token = Some(token.clone());
        tracing::debug!(
            token = %token.0,
            context_id = %scope.context_id(),
            function_name = function_name,
            "invoke_js_function_stream: created token and prelude"
        );
        let scope_prelude = super::build_scope_prelude(scope, &token_prelude)?;

        // For stream requests, we start the async function but DON'T wait for promise resolution.
        // The function yields chunks via __chat_yield() and the promise never resolves (by design).
        // We just need to ensure the function starts executing and can yield chunks.
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
                    const args = {};
                    const func = globalThis["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        throw new Error("JS function not found: {}");
                    }}
                    // Preserve token in args so async handlers can pass it explicitly.
                    args.__baml_invocation_token = __baml_invocation_token;
                    // Start the async function but don't await it - it's designed to never resolve
                    // for stream requests. Chunks are collected via the yield channel.
                    func(args);
                    return JSON.stringify({{ success: true }});
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            scope_prelude, args_json, function_name, function_name
        );

        // Execute with explicit invocation scope and token prelude; native callbacks resolve scope by token.
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
        if let Err(err) = drive_event_loop(&self.runtime).await {
            tracing::warn!(error = ?err, "QuickJS drive_event_loop failed");
        }

        // Yield to tokio to allow the async function to progress
        tokio::task::yield_now().await;

        Ok(())
    }
}
