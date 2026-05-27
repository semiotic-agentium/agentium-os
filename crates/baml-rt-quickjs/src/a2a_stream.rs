//! A2A stream: per-bridge handover lane (begin → invoke → collect).
//!
//! Each [`BridgeHandle`] owns a dedicated OS thread with a `LocalSet` that runs stream,
//! invoke, and tool-invoke jobs for that bridge. Independent bridges dispatch concurrently.
//! The bridge is `!Send` so the lane uses `spawn_local`; no thread-pool blocking.
//!
//! ## Runtime invariants
//!
//! The collect loop uses `spawn_local` (tied to the LocalSet) to avoid cross-runtime
//! cancellation. The `HandoverSender` is Send so it can be sent through the mpsc channel.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<HandoverSender>();
};

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_channel as achan;
use baml_rt_core::{
    BamlRtError, Result, context::InvocationScope, stream_completion::StreamCompletion,
};
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, mpsc, mpsc::UnboundedReceiver},
    time::sleep,
};

use crate::quickjs_bridge::{QuickJSBridge, StreamSessionId};

/// Single item from the stream: either a chunk or a terminal completion.
#[derive(Debug, Clone)]
pub enum StreamOutput {
    /// Incremental chunk from JS yield (no completion yet).
    Chunk(Value),
    /// Chunk from the effect relay (tool/status); merged into the same stream for order. Router marks as toolStreamChunk.
    RelayChunk(Value),
    /// Terminal completion; payload is the accompanying value (e.g. error object or Null).
    Terminal(Value, StreamCompletion),
}

/// Resume channel: transport sends the next turn request (Value) so the collector can deliver it
/// into the same JS run. Only used for live stream sessions that may suspend on InputRequired.
pub type ResumeTx = mpsc::Sender<Value>;
pub type ResumeRx = mpsc::Receiver<Value>;

// --- Same-thread session (main-style): begin → invoke → collect ---

/// Session after yield buffer is ready; invoke not yet called.
pub struct A2aYieldSessionReady<'a> {
    bridge: &'a mut QuickJSBridge,
}

/// Session after stream invoked; ready for collect.
pub struct A2aYieldSessionComplete<'a> {
    #[expect(
        dead_code,
        reason = "bridge handle retained for the session-complete lifetime; not read on this path"
    )]
    bridge: &'a mut QuickJSBridge,
    pub session_id: StreamSessionId,
    pub yield_rx: UnboundedReceiver<Value>,
}

/// Begins a stream session (setup yield buffer). Caller must then call [`A2aYieldSessionReady::invoke`].
pub async fn begin_a2a_yield_session(
    bridge: &mut QuickJSBridge,
) -> Result<A2aYieldSessionReady<'_>> {
    bridge.setup_a2a_yield_buffer().await?;
    Ok(A2aYieldSessionReady { bridge })
}

impl<'a> A2aYieldSessionReady<'a> {
    /// Invokes `onChatMessage` with the given request. Caller must then run collect (e.g. [`collect_into_channel_owned`]).
    pub async fn invoke(
        self,
        scope: &InvocationScope,
        request: Value,
    ) -> Result<A2aYieldSessionComplete<'a>> {
        let (session_id, yield_rx) = self
            .bridge
            .invoke_js_function_stream(scope, "onChatMessage", request)
            .await?;
        Ok(A2aYieldSessionComplete {
            bridge: self.bridge,
            session_id,
            yield_rx,
        })
    }
}

/// Job for the per-bridge handover lane: stream (begin→invoke→collect), one-shot invoke, or tool call.
pub(crate) enum HandoverJob {
    Stream(StreamHandoverRequest),
    Invoke {
        bridge: Arc<Mutex<QuickJSBridge>>,
        scope: InvocationScope,
        request: Value,
        tx_result:
            tokio::sync::oneshot::Sender<std::result::Result<Value, baml_rt_core::BamlRtError>>,
    },
    /// Generic named-function invocation (used by dispatch handlers).
    InvokeNamed {
        bridge: Arc<Mutex<QuickJSBridge>>,
        scope: InvocationScope,
        function_name: String,
        request: Value,
        tx_result:
            tokio::sync::oneshot::Sender<std::result::Result<Value, baml_rt_core::BamlRtError>>,
    },
    /// Optional named-function invocation — returns None if not registered.
    InvokeOptional {
        bridge: Arc<Mutex<QuickJSBridge>>,
        scope: InvocationScope,
        function_name: String,
        request: Value,
        tx_result: tokio::sync::oneshot::Sender<
            std::result::Result<Option<Value>, baml_rt_core::BamlRtError>,
        >,
    },
    ToolInvoke {
        bridge: Arc<Mutex<QuickJSBridge>>,
        scope: InvocationScope,
        tool_name: String,
        input: Value,
        tx_result:
            tokio::sync::oneshot::Sender<std::result::Result<Value, baml_rt_core::BamlRtError>>,
    },
    /// Deliver a resume input to a suspended stream session. Used by the collect loop (running
    /// on a regular tokio::spawn) to dispatch the !Send runtime.eval() onto the LocalSet thread.
    DeliverResume {
        bridge: Arc<Mutex<QuickJSBridge>>,
        session_id: crate::quickjs_bridge::StreamSessionId,
        message: Value,
        tx_result: tokio::sync::oneshot::Sender<std::result::Result<(), baml_rt_core::BamlRtError>>,
    },
}

pub(crate) struct StreamHandoverRequest {
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
    tx_err: mpsc::Sender<StreamOutput>,
    tx_stream: mpsc::Sender<StreamOutput>,
    abort_rx: mpsc::Receiver<()>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
}

/// Per-bridge handover handle: owns the dedicated OS thread + `LocalSet` that
/// runs stream, invoke, and tool-invoke jobs for one `QuickJSBridge`.
///
/// Callers hold `Arc<BridgeHandle>`. Direct bridge locking is available via
/// [`bridge()`](BridgeHandle::bridge); handover dispatch uses the internal
/// sender (accessed through the free functions in this module).
///
/// **Drop semantics (cancel, not drain):** closing the sender makes the lane
/// thread exit its recv loop → `LocalSet` dropped → in-flight `spawn_local`
/// tasks cancelled → thread exits → `join()` returns. JS on the QuickJS worker
/// thread may continue as a zombie; yield sends fail gracefully
/// (`stream.rs:104`).
pub struct BridgeHandle {
    bridge: Arc<Mutex<QuickJSBridge>>,
    handover_tx: achan::Sender<HandoverJob>,
    lane_thread: Option<std::thread::JoinHandle<()>>,
}

impl BridgeHandle {
    /// Create a new per-bridge handover lane.
    ///
    /// Spawns a dedicated OS thread with a `LocalSet` + current-thread tokio
    /// runtime. The thread processes `HandoverJob`s until the sender is closed.
    ///
    /// `label` is used in the thread name for debuggability (e.g. agent ID).
    pub fn new(bridge: Arc<Mutex<QuickJSBridge>>, label: &str) -> Self {
        let (tx, rx) = achan::unbounded::<HandoverJob>();
        let tx_for_lane = tx.clone(); // passed into the lane so collect loops can dispatch resume evals
        let thread_name = format!("baml-handover-{label}");
        let handle = std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || {
                let lane_tx = tx_for_lane;
                tracing::debug!(thread = %thread_name, "handover lane: thread started");
                // Multi-threaded runtime so spawn_blocking can use the thread pool and
                // concurrent LLM/IO futures (spawned by __baml_invoke promise bodies) can
                // run on worker threads while the LocalSet drives !Send JS tasks on this thread.
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("failed to build handover runtime");
                let local = tokio::task::LocalSet::new();
                runtime.block_on(local.run_until(async move {
                    while let Ok(job) = rx.recv().await {
                        let lane_tx_job = lane_tx.clone();
                        tokio::task::spawn_local(async move {
                            match job {
                                HandoverJob::Stream(s) => {
                                    run_stream_same_thread(
                                        s.bridge,
                                        s.scope,
                                        s.request,
                                        s.tx_err,
                                        s.tx_stream,
                                        s.abort_rx,
                                        s.resume_rx,
                                        s.relay_rx,
                                        lane_tx_job,
                                    )
                                    .await;
                                }
                                HandoverJob::Invoke {
                                    bridge,
                                    scope,
                                    request,
                                    tx_result,
                                } => {
                                    let out = run_invoke_same_thread(bridge, scope, request).await;
                                    if tx_result.send(out).is_err() {
                                        tracing::debug!("handover Invoke: result receiver dropped (task cancelled)");
                                    }
                                }
                                HandoverJob::InvokeNamed {
                                    bridge,
                                    scope,
                                    function_name,
                                    request,
                                    tx_result,
                                } => {
                                    let out = run_named_invoke_same_thread(
                                        bridge,
                                        scope,
                                        &function_name,
                                        request,
                                    )
                                    .await;
                                    if tx_result.send(out).is_err() {
                                        tracing::debug!("handover InvokeNamed: result receiver dropped (task cancelled)");
                                    }
                                }
                                HandoverJob::InvokeOptional {
                                    bridge,
                                    scope,
                                    function_name,
                                    request,
                                    tx_result,
                                } => {
                                    let out = run_optional_invoke_same_thread(
                                        bridge,
                                        scope,
                                        &function_name,
                                        request,
                                    )
                                    .await;
                                    if tx_result.send(out).is_err() {
                                        tracing::debug!("handover InvokeOptional: result receiver dropped (task cancelled)");
                                    }
                                }
                                HandoverJob::ToolInvoke {
                                    bridge,
                                    scope,
                                    tool_name,
                                    input,
                                    tx_result,
                                } => {
                                    let out = run_tool_invoke_same_thread(
                                        bridge, scope, &tool_name, input,
                                    )
                                    .await;
                                    if tx_result.send(out).is_err() {
                                        tracing::debug!("handover ToolInvoke: result receiver dropped (task cancelled)");
                                    }
                                }
                                HandoverJob::DeliverResume {
                                    bridge,
                                    session_id,
                                    message,
                                    tx_result,
                                } => {
                                    let out = QuickJSBridge::deliver_resume_input(
                                        bridge, session_id, message,
                                    )
                                    .await;
                                    if tx_result.send(out).is_err() {
                                        tracing::warn!(session_id = %session_id, "handover DeliverResume: result receiver dropped (task cancelled); resume result lost");
                                    }
                                }
                            }
                        });
                    }
                }));
                tracing::debug!("handover lane: receiver closed, thread exiting");
            })
            .expect("failed to spawn handover lane thread");
        Self {
            bridge,
            handover_tx: tx,
            lane_thread: Some(handle),
        }
    }

    /// Access the underlying bridge for direct locking.
    pub fn bridge(&self) -> &Arc<Mutex<QuickJSBridge>> {
        &self.bridge
    }

    /// Crate-internal: access the handover sender for dispatching jobs.
    pub(crate) fn handover_sender(&self) -> &achan::Sender<HandoverJob> {
        &self.handover_tx
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        // Close sender first so the lane thread's recv loop exits.
        self.handover_tx.close();
        if let Some(handle) = self.lane_thread.take()
            && let Err(e) = handle.join()
        {
            // A panic in the lane thread means any in-flight stream sessions
            // on this bridge lost their terminal chunk. Log for diagnostics.
            tracing::error!(error = ?e, "handover lane thread panicked; in-flight stream sessions may have silently incomplete streams");
        }
    }
}

/// Runs a single onChatMessage invoke using the brief-lock pattern.
///
/// Acquires the bridge lock only for synchronous setup (`prepare_brief_poll_eval`),
/// then releases it before polling. This allows concurrent contexts to make progress
/// while this invocation awaits LLM or BAML results.
async fn run_invoke_same_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
) -> std::result::Result<Value, baml_rt_core::BamlRtError> {
    QuickJSBridge::invoke_js_function_nonblocking(bridge, &scope, "onChatMessage", request).await
}

async fn run_named_invoke_same_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    function_name: &str,
    request: Value,
) -> std::result::Result<Value, baml_rt_core::BamlRtError> {
    QuickJSBridge::invoke_js_function_nonblocking(bridge, &scope, function_name, request).await
}

async fn run_optional_invoke_same_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    function_name: &str,
    request: Value,
) -> std::result::Result<Option<Value>, baml_rt_core::BamlRtError> {
    QuickJSBridge::invoke_optional_js_function_nonblocking(bridge, &scope, function_name, request)
        .await
}

/// Enqueues a named JS function invoke to the bridge's handover lane and waits for the result.
pub async fn invoke_js_function_handover(
    handle: &BridgeHandle,
    scope: InvocationScope,
    function_name: impl Into<String>,
    request: Value,
) -> Result<Value> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();
    handle
        .handover_sender()
        .send(HandoverJob::InvokeNamed {
            bridge: handle.bridge().clone(),
            scope,
            function_name: function_name.into(),
            request,
            tx_result,
        })
        .await
        .map_err(|_| {
            baml_rt_core::BamlRtError::InvalidArgument("handover lane closed".to_string())
        })?;
    rx_result.await.map_err(|_| {
        baml_rt_core::BamlRtError::InvalidArgument("handover invoke dropped".to_string())
    })?
}

/// Enqueues a single optional JS function invoke to the bridge's handover lane and waits for the result.
pub async fn invoke_optional_js_function_handover(
    handle: &BridgeHandle,
    scope: InvocationScope,
    function_name: impl Into<String>,
    request: Value,
) -> Result<Option<Value>> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();
    handle
        .handover_sender()
        .send(HandoverJob::InvokeOptional {
            bridge: handle.bridge().clone(),
            scope,
            function_name: function_name.into(),
            request,
            tx_result,
        })
        .await
        .map_err(|_| {
            baml_rt_core::BamlRtError::InvalidArgument("handover lane closed".to_string())
        })?;
    rx_result.await.map_err(|_| {
        baml_rt_core::BamlRtError::InvalidArgument("handover invoke dropped".to_string())
    })?
}

/// Runs a single JS tool invoke using the brief-lock pattern.
///
/// Acquires the bridge lock only for synchronous setup (`prepare_brief_poll_eval`),
/// then releases it before polling. This allows concurrent contexts to make progress
/// while this invocation awaits async tool execution.
async fn run_tool_invoke_same_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    tool_name: &str,
    input: Value,
) -> std::result::Result<Value, baml_rt_core::BamlRtError> {
    QuickJSBridge::invoke_js_tool_nonblocking(bridge, &scope, tool_name, input).await
}

/// Runs begin → invoke → collect entirely on the LocalSet.
///
/// **Architecture:** The setup phase (begin + invoke) requires `&mut QuickJSBridge` and
/// stays on the LocalSet. The collect loop uses the two-phase drain (brief lock →
/// `spawn_blocking(run_pending_jobs)` → channel drain) which yields the LocalSet thread
/// during the blocking phase — allowing other sessions' setups to interleave.
///
/// Using `spawn_local` (not `tokio::spawn`) keeps the collect loop tied to the handover
/// lane's LocalSet lifetime, avoiding cross-runtime cancellation that silently drops the
/// stream before content is delivered.
#[expect(
    clippy::too_many_arguments,
    reason = "same-thread stream runner threads all channel/scope inputs explicitly"
)]
async fn run_stream_same_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
    tx_err: mpsc::Sender<StreamOutput>,
    tx: mpsc::Sender<StreamOutput>,
    abort_rx: mpsc::Receiver<()>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
    handover_tx: achan::Sender<HandoverJob>,
) {
    // SETUP PHASE: brief lock, runs on the LocalSet (requires &mut QuickJSBridge).
    let context_id_log = scope.context_id().to_string();
    let (session_id, yield_rx) = {
        let mut guard = bridge.lock().await;
        let ready = match begin_a2a_yield_session(&mut guard).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(context_id = %context_id_log, error = %e, "stream setup: begin_a2a_yield_session failed");
                if tx_err
                    .send(StreamOutput::Terminal(
                        json!({ "error": e.to_string() }),
                        StreamCompletion::SemanticFinal,
                    ))
                    .await
                    .is_err()
                {
                    tracing::debug!(context_id = %context_id_log, "stream setup: error channel closed before setup error could be sent");
                }
                return;
            }
        };
        match ready.invoke(&scope, request).await {
            Ok(complete) => (complete.session_id, complete.yield_rx),
            Err(e) => {
                tracing::warn!(context_id = %context_id_log, error = %e, "stream setup: invoke onChatMessage failed");
                if tx_err
                    .send(StreamOutput::Terminal(
                        json!({ "error": e.to_string() }),
                        StreamCompletion::SemanticFinal,
                    ))
                    .await
                    .is_err()
                {
                    tracing::debug!(context_id = %context_id_log, "stream setup: error channel closed before invoke error could be sent");
                }
                return;
            }
        }
        // guard dropped — LocalSet thread free for the next session's setup
    };

    // COLLECT PHASE: runs on the LocalSet via spawn_local.
    // The two-phase drain (brief lock → spawn_blocking → channel drain) yields the LocalSet
    // thread during spawn_blocking, allowing other sessions' setup phases to interleave.
    // Using spawn_local (not tokio::spawn) ties the collect lifetime to the handover lane's
    // LocalSet, preventing cross-runtime cancellation that drops the stream silently.
    let tx_err_collect = tx_err.clone();
    let collect_handle = tokio::task::spawn_local(async move {
        let collect_result = collect_into_channel_owned_inner(
            bridge,
            session_id,
            yield_rx,
            tx,
            abort_rx,
            resume_rx,
            relay_rx,
            scope,
            handover_tx,
        )
        .await;
        if let Err(ref e) = collect_result
            && tx_err_collect
                .send(StreamOutput::Terminal(
                    json!({ "error": e.to_string() }),
                    StreamCompletion::SemanticFinal,
                ))
                .await
                .is_err()
        {
            tracing::warn!(error = %e, "collect: stream error channel closed; terminal error chunk lost");
        }
    });
    // Detach the collect handle — the idle-timeout and terminal-chunk mechanism ensures
    // the consumer is not left hanging even if a panic occurs (collect loop handles it).
    drop(collect_handle);
}

/// Enqueues stream to the bridge's handover lane (begin → invoke → collect). Returns immediately; no blocking.
pub async fn spawn_stream_handover(
    handle: &BridgeHandle,
    scope: InvocationScope,
    request: Value,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
) -> (mpsc::Receiver<StreamOutput>, mpsc::Sender<()>) {
    let (tx, rx) = mpsc::channel(64);
    let (abort_tx, abort_rx) = mpsc::channel(1);
    let tx_err = tx.clone();
    let tx_stream = tx.clone();
    let enqueue = handle
        .handover_sender()
        .send(HandoverJob::Stream(StreamHandoverRequest {
            bridge: handle.bridge().clone(),
            scope,
            request,
            tx_err,
            tx_stream,
            abort_rx,
            resume_rx,
            relay_rx,
        }))
        .await;
    if enqueue.is_err() {
        tracing::error!("stream handover: enqueue failed (lane closed)");
        let _ = tx.try_send(StreamOutput::Terminal(
            json!({ "error": "stream handover lane closed" }),
            StreamCompletion::SemanticFinal,
        ));
    }
    (rx, abort_tx)
}

/// Enqueues a single onChatMessage invoke to the bridge's handover lane and waits for the result.
pub async fn invoke_handler_handover(
    handle: &BridgeHandle,
    scope: InvocationScope,
    request: Value,
) -> Result<Value> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();
    handle
        .handover_sender()
        .send(HandoverJob::Invoke {
            bridge: handle.bridge().clone(),
            scope,
            request,
            tx_result,
        })
        .await
        .map_err(|_| {
            baml_rt_core::BamlRtError::InvalidArgument("handover lane closed".to_string())
        })?;
    rx_result.await.map_err(|_| {
        baml_rt_core::BamlRtError::InvalidArgument("handover invoke dropped".to_string())
    })?
}

/// Enqueues a single JS tool invoke to the bridge's handover lane and waits for the result.
pub async fn invoke_tool_handover(
    handle: &BridgeHandle,
    scope: InvocationScope,
    tool_name: String,
    input: Value,
) -> Result<Value> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();
    handle
        .handover_sender()
        .send(HandoverJob::ToolInvoke {
            bridge: handle.bridge().clone(),
            scope,
            tool_name,
            input,
            tx_result,
        })
        .await
        .map_err(|_| {
            baml_rt_core::BamlRtError::InvalidArgument("handover lane closed".to_string())
        })?;
    rx_result.await.map_err(|_| {
        baml_rt_core::BamlRtError::InvalidArgument("handover tool invoke dropped".to_string())
    })?
}

/// Opaque capability token for dispatching work to the handover lane from
/// [`collect_into_channel_owned`]. Produced by [`BridgeHandle`] for live paths
/// and by [`HandoverSender::noop()`] for tests that don't exercise multi-turn resume.
pub struct HandoverSender(achan::Sender<HandoverJob>);

impl HandoverSender {
    /// A permanently-closed, no-op sender.
    ///
    /// Pass this to [`collect_into_channel_owned`] when testing single-turn streams
    /// that never exercise the multi-turn resume path (`awaitInput`).
    ///
    /// **Do not use in production paths** — any multi-turn resume attempt will fail
    /// with "handover lane closed".
    pub fn noop() -> Self {
        let (tx, _rx) = achan::unbounded::<HandoverJob>();
        tx.close();
        Self(tx)
    }

    /// Extract the raw sender. Used internally to pass through the crate boundary.
    pub(crate) fn into_raw(self) -> achan::Sender<HandoverJob> {
        self.0
    }
}

/// Dispatches `deliver_resume_input` to the handover lane so the !Send `runtime.eval()` call
/// runs on the LocalSet thread rather than in the `tokio::spawn` collect loop.
///
/// The caller only awaits a `oneshot::Receiver` (Send), making the collect loop fully Send.
pub(crate) async fn deliver_resume_via_handover(
    handover_tx: &achan::Sender<HandoverJob>,
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: crate::quickjs_bridge::StreamSessionId,
    message: Value,
) -> std::result::Result<(), baml_rt_core::BamlRtError> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();
    handover_tx
        .send(HandoverJob::DeliverResume {
            bridge,
            session_id,
            message,
            tx_result,
        })
        .await
        .map_err(|_| {
            baml_rt_core::BamlRtError::InvalidArgument("handover lane closed".to_string())
        })?;
    rx_result.await.map_err(|_| {
        baml_rt_core::BamlRtError::InvalidArgument("resume deliver dropped".to_string())
    })?
}

/// Reads task state from a stream chunk. Supports object and stringified JSON shapes;
/// parse failure is treated as non-terminal state.
fn chunk_state(chunk: &Value) -> Option<String> {
    fn from_val(val: &Value) -> Option<String> {
        val.get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .map(String::from)
    }
    fn from_maybe_string(val: &Value) -> Option<String> {
        from_val(val).or_else(|| {
            val.as_str().and_then(|s| {
                match serde_json::from_str::<Value>(s) {
                    Ok(parsed) => from_val(&parsed),
                    Err(e) => {
                        tracing::trace!(error = %e, "chunk_state: stringified task/statusUpdate parse failed");
                        None
                    }
                }
            })
        })
    }
    /// Match wire variants: `statusUpdate.status.state` and nested relay
    /// `statusUpdate.status_update.status.state` (see web `getStateFromChunk`).
    fn from_status_update_container(su: &Value) -> Option<String> {
        from_maybe_string(su).or_else(|| {
            su.get("status_update")
                .or_else(|| su.get("statusUpdate"))
                .and_then(from_maybe_string)
        })
    }
    chunk
        .get("task")
        .and_then(from_maybe_string)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(from_status_update_container)
        })
        .or_else(|| {
            chunk
                .get("status_update")
                .and_then(from_status_update_container)
        })
}

fn chunk_has_final_state(chunk: &Value) -> bool {
    if chunk.get("final").and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    matches!(
        chunk_state(chunk).as_deref(),
        Some("TASK_STATE_COMPLETED") | Some("TASK_STATE_FAILED")
    )
}

fn chunk_has_input_required_state(chunk: &Value) -> bool {
    matches!(
        chunk_state(chunk).as_deref(),
        Some("TASK_STATE_INPUT_REQUIRED")
    )
}

/// Build a completion chunk when the yield channel closes without an explicit terminal yield.
/// The collector is the layer that knows the stream ended; this chunk flows through the normal
/// pipeline so provenance records task_execution_ended. Not synthetic—observable stream lifecycle.
fn make_channel_closed_completion_chunk(
    last_chunk: Option<&Value>,
    scope: &InvocationScope,
    completion: StreamCompletion,
) -> Value {
    let (task_id, context_id) = match last_chunk {
        Some(c) => {
            let tid = c
                .get("task")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    c.get("statusUpdate")
                        .and_then(|s| s.get("status"))
                        .and_then(|s| s.get("taskId"))
                        .and_then(Value::as_str)
                        .map(String::from)
                });
            let cid = c
                .get("task")
                .and_then(|t| t.get("contextId"))
                .and_then(Value::as_str)
                .map(String::from);
            (
                tid.unwrap_or_else(|| {
                    scope
                        .task_id_opt()
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_else(|| format!("stream-{}", scope.context_id()))
                }),
                cid.unwrap_or_else(|| scope.context_id().as_str().to_string()),
            )
        }
        None => (
            scope
                .task_id_opt()
                .map(|t| t.as_str().to_string())
                .unwrap_or_else(|| format!("stream-{}", scope.context_id())),
            scope.context_id().as_str().to_string(),
        ),
    };
    let (state, message, error_reason): (&str, Option<Value>, Option<&'static str>) =
        match completion {
            StreamCompletion::Timeout => (
                "TASK_STATE_FAILED",
                Some(json!({
                    "parts": [{ "text": "Request timed out before the agent produced a terminal response." }]
                })),
                Some("request_timed_out_before_agent_response"),
            ),
            StreamCompletion::ChannelClosed => ("TASK_STATE_COMPLETED", None, None),
            _ => ("TASK_STATE_COMPLETED", None, None),
        };
    let mut status = json!({ "state": state });
    if let Some(msg) = message {
        status["message"] = msg;
    }
    if let Some(reason) = error_reason {
        status["error_reason"] = json!(reason);
    }
    json!({
        "task": {
            "id": task_id,
            "contextId": context_id,
            "status": status
        }
    })
}

/// Common state for a stream collection pass.
///
/// Split into explicit phase types to keep lock ownership and control flow visible.
enum CollectIteration {
    /// Continue pumping. When `had_chunks` is true, use minimal sleep to reduce latency.
    Continue {
        had_chunks: bool,
    },
    Done,
}

struct StreamCollectorContext {
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: StreamSessionId,
    yield_rx: UnboundedReceiver<Value>,
    tx: mpsc::Sender<StreamOutput>,
    abort_rx: mpsc::Receiver<()>,
    /// When present, InputRequired suspends instead of finalizing; collector blocks on resume and delivers into same JS run.
    resume_rx: Option<ResumeRx>,
    /// When present, drain each iteration after yield_rx and emit as RelayChunk (single ordered stream).
    relay_rx: Option<mpsc::Receiver<Value>>,
    /// Invocation scope; used to build completion chunks when channel closes without explicit terminal yield.
    scope: InvocationScope,
    /// Idle timeout in seconds; reset on every yield. Stream ends with Timeout if no yield for this long.
    idle_timeout_secs: u64,
    /// Last time we saw a chunk from the agent; used for idle timeout (reset per yield).
    last_yield_at: Instant,
    all: Vec<Value>,
    interval: Duration,
    finalized: bool,
    /// Handover sender used to dispatch resume evals onto the LocalSet thread (making the collect
    /// loop fully Send so it can run via tokio::spawn rather than spawn_local).
    handover_tx: achan::Sender<HandoverJob>,
}

impl StreamCollectorContext {
    async fn finalize_once(&mut self, completion: Option<StreamCompletion>) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let _ = self
            .bridge
            .lock()
            .await
            .finalize_a2a_stream_invocation(self.session_id)
            .await;

        if let Some(completion) = completion {
            // When channel closes or times out without explicit terminal yield, emit a completion
            // chunk so provenance records task_execution_ended. The collector knows the stream
            // ended; this is canonical, not synthetic.
            let last_state = self.all.last().and_then(chunk_state);
            let last_has_terminal_state = matches!(
                last_state.as_deref(),
                Some(
                    "TASK_STATE_COMPLETED"
                        | "TASK_STATE_FAILED"
                        | "TASK_STATE_REJECTED"
                        | "TASK_STATE_CANCELED"
                )
            );
            let payload = match completion {
                StreamCompletion::ChannelClosed | StreamCompletion::Timeout => {
                    make_channel_closed_completion_chunk(self.all.last(), &self.scope, completion)
                }
                StreamCompletion::SemanticFinal if !last_has_terminal_state => {
                    make_channel_closed_completion_chunk(self.all.last(), &self.scope, completion)
                }
                _ => Value::Null,
            };
            let _ = self
                .tx
                .send(StreamOutput::Terminal(payload, completion))
                .await;
        }
    }

    /// Handle InputRequired terminal: either block on resume and deliver into JS (true resume)
    /// or finalize with InputRequired (standalone/test).
    async fn handle_input_required_resume(&mut self) -> Result<CollectIteration> {
        if let Some(ref mut resume_rx) = self.resume_rx {
            if self
                .tx
                .send(StreamOutput::Terminal(
                    Value::Null,
                    StreamCompletion::InputRequired,
                ))
                .await
                .is_err()
            {
                self.finalize_once(None).await;
                return Ok(CollectIteration::Done);
            }
            let message = tokio::select! {
                msg = resume_rx.recv() => match msg {
                    Some(m) => m,
                    None => {
                        self.finalize_once(Some(StreamCompletion::ChannelClosed)).await;
                        return Ok(CollectIteration::Done);
                    }
                },
                _ = self.abort_rx.recv() => {
                    self.finalize_once(Some(StreamCompletion::ChannelClosed)).await;
                    return Ok(CollectIteration::Done);
                }
            };
            // Dispatch the !Send runtime.eval() call to the LocalSet thread via the handover lane
            // so this collect loop (running on tokio::spawn) remains fully Send.
            let deliver_result = deliver_resume_via_handover(
                &self.handover_tx,
                self.bridge.clone(),
                self.session_id,
                message,
            )
            .await;
            if let Err(e) = deliver_result {
                tracing::warn!(session_id = %self.session_id, error = %e, "collect: deliver_resume_input failed");
                let _ = self
                    .tx
                    .send(StreamOutput::Terminal(
                        serde_json::json!({ "error": e.to_string() }),
                        StreamCompletion::SemanticFinal,
                    ))
                    .await;
                self.finalize_once(Some(StreamCompletion::SemanticFinal))
                    .await;
                return Ok(CollectIteration::Done);
            }
            return Ok(CollectIteration::Continue { had_chunks: true });
        }
        self.finalize_once(Some(StreamCompletion::InputRequired))
            .await;
        Ok(CollectIteration::Done)
    }

    async fn next_iteration(&mut self) -> Result<CollectIteration> {
        if self.abort_rx.try_recv().is_ok() {
            self.finalize_once(Some(StreamCompletion::ChannelClosed))
                .await;
            return Ok(CollectIteration::Done);
        }
        // Two-phase drain: run pending JS jobs WITHOUT holding the bridge lock (so concurrent
        // contexts can acquire it for their own setup/invoke), then briefly lock to drain the channel.
        //
        // Phase 1: get the runtime Arc (brief lock), then run pending jobs via spawn_blocking
        // while the lock is RELEASED. This allows other spawn_local tasks to make progress.
        let rt_arc = {
            let guard = self.bridge.lock().await;
            Arc::clone(guard.runtime())
        }; // bridge lock released here
        tokio::task::spawn_blocking(move || {
            rt_arc.exe_rt_task_in_event_loop(|r| r.run_pending_jobs_if_any());
        })
        .await
        .map_err(|e| BamlRtError::QuickJs(format!("collect drain spawn_blocking: {e}")))?;

        // Phase 2: drain the yield channel — no bridge lock needed since yield_rx is owned here.
        struct DrainResult {
            chunks: Vec<Value>,
            channel_closed: bool,
        }
        let drain = {
            use tokio::sync::mpsc::error::TryRecvError;
            let mut chunks = Vec::new();
            let mut channel_closed = false;
            loop {
                match self.yield_rx.try_recv() {
                    Ok(value) => chunks.push(value),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        channel_closed = true;
                        break;
                    }
                }
            }
            Ok::<DrainResult, baml_rt_core::BamlRtError>(DrainResult {
                chunks,
                channel_closed,
            })
        };
        let drain = match drain {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    "collect: drain_yield_buffer failed; finalizing stream invocation"
                );
                self.finalize_once(None).await;
                return Err(e);
            }
        };

        // Relay tool/status chunks first (causal order).
        let mut saw_relay = false;
        if let Some(ref mut relay_rx) = self.relay_rx {
            while let Ok(value) = relay_rx.try_recv() {
                saw_relay = true;
                if self.tx.send(StreamOutput::RelayChunk(value)).await.is_err() {
                    break;
                }
            }
        }

        let mut saw_yield = false;
        for value in drain.chunks {
            saw_yield = true;
            let has_input_req = chunk_has_input_required_state(&value);
            let has_final = chunk_has_final_state(&value);
            if self
                .tx
                .send(StreamOutput::Chunk(value.clone()))
                .await
                .is_err()
            {
                self.finalize_once(None).await;
                return Ok(CollectIteration::Done);
            }
            self.all.push(value);
            if has_input_req {
                return self.handle_input_required_resume().await;
            }
            if has_final {
                self.finalize_once(Some(StreamCompletion::SemanticFinal))
                    .await;
                return Ok(CollectIteration::Done);
            }
        }
        if saw_yield {
            self.last_yield_at = Instant::now();
        }

        if let Some(ref mut relay_rx) = self.relay_rx {
            while let Ok(value) = relay_rx.try_recv() {
                saw_relay = true;
                if self.tx.send(StreamOutput::RelayChunk(value)).await.is_err() {
                    break;
                }
            }
        }

        if drain.channel_closed {
            self.finalize_once(Some(StreamCompletion::ChannelClosed))
                .await;
            return Ok(CollectIteration::Done);
        }

        let idle_elapsed = self.last_yield_at.elapsed();
        if self.idle_timeout_secs > 0 && idle_elapsed >= Duration::from_secs(self.idle_timeout_secs)
        {
            tracing::warn!(
                session_id = %self.session_id,
                idle_secs = self.idle_timeout_secs,
                elapsed_secs = idle_elapsed.as_secs(),
                "collect: idle timeout (no yield); finalizing with Timeout"
            );
            self.finalize_once(Some(StreamCompletion::Timeout)).await;
            return Ok(CollectIteration::Done);
        }

        Ok(CollectIteration::Continue {
            had_chunks: saw_yield || saw_relay,
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "stream collection requires all eight independent channel/scope parameters"
)]
pub async fn collect_into_channel_owned(
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: StreamSessionId,
    yield_rx: UnboundedReceiver<Value>,
    tx: mpsc::Sender<StreamOutput>,
    abort_rx: mpsc::Receiver<()>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
    scope: InvocationScope,
    handover_tx: HandoverSender,
) -> Result<()> {
    collect_into_channel_owned_inner(
        bridge,
        session_id,
        yield_rx,
        tx,
        abort_rx,
        resume_rx,
        relay_rx,
        scope,
        handover_tx.into_raw(),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "each parameter is an independent owned resource with no meaningful grouping"
)]
async fn collect_into_channel_owned_inner(
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: StreamSessionId,
    yield_rx: UnboundedReceiver<Value>,
    tx: mpsc::Sender<StreamOutput>,
    abort_rx: mpsc::Receiver<()>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
    scope: InvocationScope,
    handover_tx: achan::Sender<HandoverJob>,
) -> Result<()> {
    let now = std::time::Instant::now();
    let idle_timeout_secs = {
        let guard = bridge.lock().await;
        guard.stream_collector_idle_secs()
    };
    let mut context = StreamCollectorContext {
        bridge,
        session_id,
        yield_rx,
        tx,
        abort_rx,
        resume_rx,
        relay_rx,
        scope,
        idle_timeout_secs,
        last_yield_at: now,
        all: Vec::new(),
        interval: Duration::from_millis(20),
        finalized: false,
        handover_tx,
    };

    loop {
        match context.next_iteration().await? {
            CollectIteration::Continue { had_chunks } => {
                // When we received chunks this iteration, use minimal sleep to reduce latency.
                // Idle iterations use full interval to avoid busy-looping.
                let interval = if had_chunks {
                    Duration::from_millis(1)
                } else {
                    context.interval
                };
                sleep(interval).await;
            }
            CollectIteration::Done => return Ok(()),
        }
    }
}
