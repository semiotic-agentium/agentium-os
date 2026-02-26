//! A2A stream handover and pump for interleaved execution.
//!
//! Stream routing follows one production path:
//! `spawn_stream_handover` → `collect_into_channel_owned` (pump with server-side idle timeout).
//! The pump forwards chunks until terminal state (final/input_required), channel close, or
//! idle timeout. Idle timeout is reset on every yield; configurable via `stream_collector_idle_secs`.
//!
//! ## Interleaved streaming invariants (canonical)
//!
//! 1. Single handover path.
//!    Every streaming request starts with `spawn_stream_handover` and is drained by
//!    `collect_into_channel_owned`; no alternate collector path is used in production.
//!
//! 2. Context routing discipline.
//!    Stream yield routing is resolved from
//!    active invocation context and stream session state, not mutable global runtime state.
//!
//! 3. Single advancement lane.
//!    At most one collector may advance QuickJS pending jobs at a time
//!    via the coordinator; all other steps are short per-iteration operations.
//!
//! 4. Progress loop discipline.
//!    Each iteration is bounded: advance jobs, read/drain chunks,
//!    evaluate terminal state, and yield before retrying.
//!
//! 5. Single terminalization.
//!    Completion happens only via one terminal reason
//!    (`final`, completed/failed, input required, channel close, idle timeout) and a single
//!    idempotent finalization path.
//!
//! Formalized stream architecture and invariants are documented in this crate's `README.md`.

use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use async_channel as achan;
use baml_rt_core::{Result, context::InvocationScope, stream_completion::StreamCompletion};
use serde_json::Value;
use tokio::{
    sync::{
        Mutex, Semaphore, mpsc,
        mpsc::{UnboundedReceiver, error::TryRecvError},
    },
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

static STREAM_PENDING_JOB_COORDINATOR: OnceLock<Semaphore> = OnceLock::new();
struct StreamHandoverRequest {
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
    tx_err: mpsc::Sender<StreamOutput>,
    tx_stream: mpsc::Sender<StreamOutput>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
}

static STREAM_HANDOVER_LANE: OnceLock<achan::Sender<StreamHandoverRequest>> = OnceLock::new();

fn stream_pending_job_coordinator() -> &'static Semaphore {
    STREAM_PENDING_JOB_COORDINATOR.get_or_init(|| Semaphore::new(1))
}

fn stream_handover_lane() -> &'static achan::Sender<StreamHandoverRequest> {
    STREAM_HANDOVER_LANE.get_or_init(|| {
        let (tx, rx) = achan::unbounded::<StreamHandoverRequest>();
        std::thread::Builder::new()
            .name("baml-stream-handover-lane".to_string())
            .spawn(move || {
                tracing::info!("stream handover lane: thread started");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build stream handover runtime");
                let local = tokio::task::LocalSet::new();
                runtime.block_on(local.run_until(async move {
                    while let Ok(job) = rx.recv().await {
                        tracing::trace!("stream handover lane: dequeued job");
                        tokio::task::spawn_local(async move {
                            run_stream_on_js_thread(
                                job.bridge,
                                job.scope,
                                job.request,
                                job.tx_err,
                                job.tx_stream,
                                job.resume_rx,
                                job.relay_rx,
                            )
                            .await;
                        });
                    }
                }));
                tracing::warn!("stream handover lane: receiver closed, thread exiting");
            })
            .expect("failed to spawn stream handover lane thread");
        tx
    })
}

/// Resume channel: transport sends the next turn request (Value) so the collector can deliver it
/// into the same JS run. Only used for live stream sessions that may suspend on InputRequired.
pub type ResumeTx = mpsc::Sender<Value>;
pub type ResumeRx = mpsc::Receiver<Value>;

/// Runs the stream handover on the dedicated QuickJS worker thread.
///
/// Runs start + collector path for one stream session.
async fn run_stream_on_js_thread(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
    tx_err: mpsc::Sender<StreamOutput>,
    tx: mpsc::Sender<StreamOutput>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
) {
    tracing::debug!(
        context_id = %scope.context_id(),
        has_resume_rx = resume_rx.is_some(),
        has_relay_rx = relay_rx.is_some(),
        "stream handover: run_stream_on_js_thread start"
    );
    let start = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, "onChatMessage", request)
            .await
    };

    let (session_id, yield_rx) = match start {
        Ok(ok) => {
            tracing::debug!(
                context_id = %scope.context_id(),
                session_id = %ok.0,
                "stream handover: stream invocation started"
            );
            ok
        }
        Err(e) => {
            tracing::warn!(
                context_id = %scope.context_id(),
                error = %e,
                "stream handover: stream invocation failed"
            );
            let _ = tx_err
                .send(StreamOutput::Terminal(
                    serde_json::json!({ "error": e.to_string() }),
                    StreamCompletion::SemanticFinal,
                ))
                .await;
            return;
        }
    };

    if let Err(e) =
        collect_into_channel_owned(bridge, session_id, yield_rx, tx, resume_rx, relay_rx).await
    {
        let _ = tx_err
            .send(StreamOutput::Terminal(
                serde_json::json!({ "error": e.to_string() }),
                StreamCompletion::SemanticFinal,
            ))
            .await;
    }
}

/// Unified incremental handover entrypoint.
///
/// Starts `onChatMessage` stream invocation and spawns the single collector path that forwards
/// `(chunk, completion)` items to the returned receiver. When `resume_rx` is `Some`, the stream
/// emitting InputRequired will block on it and deliver the next message into the same JS run
/// (true resume). When `resume_rx` is `None` (standalone/test), the collector returns Done on
/// InputRequired so the stream completes and no one blocks.
/// When `relay_rx` is `Some`, the collector drains it each iteration (after yield_rx) and emits
/// those chunks as `StreamOutput::RelayChunk` so tool/status chunks stay in order with message chunks.
pub async fn spawn_stream_handover(
    bridge: Arc<Mutex<QuickJSBridge>>,
    scope: InvocationScope,
    request: Value,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
) -> mpsc::Receiver<StreamOutput> {
    let (tx, rx) = mpsc::channel(64);
    tracing::trace!(
        context_id = %scope.context_id(),
        has_resume_rx = resume_rx.is_some(),
        has_relay_rx = relay_rx.is_some(),
        "stream handover: enqueue request"
    );
    let tx_err = tx.clone();
    let tx_stream = tx.clone();
    let ctx_for_log = scope.context_id().to_string();
    let enqueue = stream_handover_lane()
        .send(StreamHandoverRequest {
            bridge,
            scope,
            request,
            tx_err,
            tx_stream,
            resume_rx,
            relay_rx,
        })
        .await;
    if enqueue.is_err() {
        tracing::error!("stream handover: enqueue failed (lane unavailable)");
        let _ = tx.try_send(StreamOutput::Terminal(
            serde_json::json!({ "error": "stream handover lane unavailable" }),
            StreamCompletion::SemanticFinal,
        ));
    } else {
        tracing::trace!(context_id = %ctx_for_log, "stream handover: enqueue accepted");
    }

    rx
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
    chunk
        .get("task")
        .and_then(from_maybe_string)
        .or_else(|| chunk.get("statusUpdate").and_then(from_maybe_string))
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

/// Common state for a stream collection pass.
///
/// Split into explicit phase types to keep lock ownership and control flow visible.
enum CollectIteration {
    Continue,
    Done,
}

struct StreamCollectorContext {
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: StreamSessionId,
    yield_rx: UnboundedReceiver<Value>,
    tx: mpsc::Sender<StreamOutput>,
    /// When present, InputRequired suspends instead of finalizing; collector blocks on resume and delivers into same JS run.
    resume_rx: Option<ResumeRx>,
    /// When present, drain each iteration after yield_rx and emit as RelayChunk (single ordered stream).
    relay_rx: Option<mpsc::Receiver<Value>>,
    start: Instant,
    /// Idle timeout in seconds; reset on every yield. Stream ends with Timeout if no yield for this long.
    idle_timeout_secs: u64,
    /// Last time we saw a chunk from the agent; used for idle timeout (reset per yield).
    last_yield_at: Instant,
    all: Vec<Value>,
    interval: Duration,
    finalized: bool,
}

impl StreamCollectorContext {
    async fn finalize_once(&mut self, completion: Option<StreamCompletion>) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let elapsed = self.start.elapsed();
        tracing::trace!(
            session_id = %self.session_id,
            elapsed_ms = elapsed.as_millis() as u64,
            chunk_count = self.all.len(),
            completion = ?completion,
            "collect: finalize_once"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!(
                "collect: finalize_once session={} completion={:?} elapsed_ms={} chunk_count={}",
                self.session_id,
                completion,
                elapsed.as_millis(),
                self.all.len()
            );
        }

        let _ = self
            .bridge
            .lock()
            .await
            .finalize_a2a_stream_invocation(self.session_id)
            .await;

        if let Some(completion) = completion {
            let _ = self
                .tx
                .send(StreamOutput::Terminal(Value::Null, completion))
                .await;
        }
    }

    /// Handle InputRequired terminal: either block on resume and deliver into JS (true resume)
    /// or finalize with InputRequired (standalone/test).
    async fn handle_input_required_resume(&mut self) -> Result<CollectIteration> {
        tracing::trace!(session_id = %self.session_id, "collect: terminal reason input_required");
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!(
                "collect: terminal reason input_required session={}",
                self.session_id
            );
        }
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
            tracing::trace!(session_id = %self.session_id, "collect: blocking on resume_rx");
            let message = match resume_rx.recv().await {
                Some(m) => m,
                None => {
                    tracing::debug!(session_id = %self.session_id, "collect: resume_rx closed");
                    self.finalize_once(Some(StreamCompletion::ChannelClosed))
                        .await;
                    return Ok(CollectIteration::Done);
                }
            };
            tracing::trace!(session_id = %self.session_id, "collect: resume message received, delivering into JS");
            let deliver_result =
                QuickJSBridge::deliver_resume_input(self.bridge.clone(), self.session_id, message)
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
            return Ok(CollectIteration::Continue);
        }
        self.finalize_once(Some(StreamCompletion::InputRequired))
            .await;
        Ok(CollectIteration::Done)
    }

    async fn next_iteration(&mut self) -> Result<CollectIteration> {
        tracing::trace!(
            session_id = %self.session_id,
            "collect: entering pump phase"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!("collect: entering pump phase session={}", self.session_id);
        }
        run_stream_pending_jobs(&self.bridge).await;
        tracing::trace!(
            session_id = %self.session_id,
            "collect: pump phase advanced pending jobs"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!(
                "collect: pump phase advanced pending jobs session={}",
                self.session_id
            );
        }

        tracing::trace!(
            session_id = %self.session_id,
            "stream: relay phase"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!("stream: relay phase session={}", self.session_id);
        }

        // Relay tool/status chunks first (causal order). Forward each immediately; do not buffer.
        if let Some(ref mut relay_rx) = self.relay_rx {
            while let Ok(value) = relay_rx.try_recv() {
                if self.tx.send(StreamOutput::RelayChunk(value)).await.is_err() {
                    break;
                }
            }
        }

        // Forward yield chunks one-by-one. Do not buffer: receive → forward immediately.
        let mut channel_closed = false;
        let mut saw_yield = false;
        loop {
            match self.yield_rx.try_recv() {
                Ok(value) => {
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
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    channel_closed = true;
                    break;
                }
            }
        }
        if saw_yield {
            self.last_yield_at = Instant::now();
        }

        // Relay any remaining tool/status when there were no yield chunks this iteration.
        if let Some(ref mut relay_rx) = self.relay_rx {
            while let Ok(value) = relay_rx.try_recv() {
                if self.tx.send(StreamOutput::RelayChunk(value)).await.is_err() {
                    break;
                }
            }
        }

        if channel_closed {
            tracing::trace!(session_id = %self.session_id, "stream: channel closed");
            if std::env::var("BAML_STREAM_DEBUG").is_ok() {
                eprintln!("stream: channel closed session={}", self.session_id);
            }
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

        tracing::trace!(
            session_id = %self.session_id,
            total_chunks = self.all.len(),
            idle_elapsed_secs = idle_elapsed.as_secs(),
            idle_timeout_secs = self.idle_timeout_secs,
            "collect: continuing pump"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!(
                "collect: continuing pump session={} total_chunks={} idle_elapsed_secs={}",
                self.session_id,
                self.all.len(),
                idle_elapsed.as_secs()
            );
        }

        Ok(CollectIteration::Continue)
    }
}

async fn run_stream_pending_jobs(bridge: &Arc<Mutex<QuickJSBridge>>) {
    let coordinator = stream_pending_job_coordinator();
    let Ok(_coordinator_permit) = coordinator.try_acquire() else {
        tracing::trace!("collect: run_stream_pending_jobs skipped (coordinator busy)");
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!("collect: run_stream_pending_jobs skipped (coordinator busy)");
        }
        return;
    };

    tracing::trace!("collect: run_stream_pending_jobs acquired coordinator");
    if std::env::var("BAML_STREAM_DEBUG").is_ok() {
        eprintln!("collect: run_stream_pending_jobs acquired coordinator");
    }

    let guard = match tokio::time::timeout(Duration::from_millis(50), bridge.lock()).await {
        Ok(guard) => guard,
        Err(_) => {
            tracing::trace!("collect: run_stream_pending_jobs skipped (bridge lock busy)");
            if std::env::var("BAML_STREAM_DEBUG").is_ok() {
                eprintln!("collect: run_stream_pending_jobs skipped (bridge lock busy)");
            }
            return;
        }
    };

    tracing::trace!("collect: run_stream_pending_jobs advancing pending jobs");
    if std::env::var("BAML_STREAM_DEBUG").is_ok() {
        eprintln!("collect: run_stream_pending_jobs advancing pending jobs");
    }
    guard.advance_pending_jobs();

    tracing::trace!("collect: run_stream_pending_jobs completed");
    if std::env::var("BAML_STREAM_DEBUG").is_ok() {
        eprintln!("collect: run_stream_pending_jobs completed");
    }
}

pub async fn collect_into_channel_owned(
    bridge: Arc<Mutex<QuickJSBridge>>,
    session_id: StreamSessionId,
    yield_rx: UnboundedReceiver<Value>,
    tx: mpsc::Sender<StreamOutput>,
    resume_rx: Option<ResumeRx>,
    relay_rx: Option<mpsc::Receiver<Value>>,
) -> Result<()> {
    tracing::trace!(
        session_id = ?session_id,
        "[collect] collect_into_channel_owned entered",
    );
    if std::env::var("BAML_STREAM_DEBUG").is_ok() {
        eprintln!(
            "[collect] collect_into_channel_owned entered session={}",
            session_id
        );
    }
    let start = std::time::Instant::now();
    let idle_timeout_secs = {
        let guard = bridge.lock().await;
        guard.stream_collector_idle_secs()
    };
    let mut context = StreamCollectorContext {
        bridge,
        session_id,
        yield_rx,
        tx,
        resume_rx,
        relay_rx,
        start,
        idle_timeout_secs,
        last_yield_at: start,
        all: Vec::new(),
        interval: Duration::from_millis(50),
        finalized: false,
    };
    let mut phase_generation: u64 = 0;

    loop {
        let active_session_id = context.session_id;
        tracing::trace!(
            session_id = %active_session_id,
            phase = phase_generation,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "collect: loop iteration begin"
        );
        if std::env::var("BAML_STREAM_DEBUG").is_ok() {
            eprintln!(
                "collect: loop iteration begin session={} phase={} elapsed_ms={}",
                active_session_id,
                phase_generation,
                start.elapsed().as_millis()
            );
        }
        match context.next_iteration().await? {
            CollectIteration::Continue => {
                let interval = context.interval;
                tracing::trace!(
                    session_id = %active_session_id,
                    phase = phase_generation,
                    interval_ms = interval.as_millis() as u64,
                    "collect: loop scheduled continue"
                );
                if std::env::var("BAML_STREAM_DEBUG").is_ok() {
                    eprintln!(
                        "collect: loop scheduled continue session={} phase={} interval_ms={}",
                        active_session_id,
                        phase_generation,
                        interval.as_millis()
                    );
                }
                sleep(interval).await;
            }
            CollectIteration::Done => return Ok(()),
        }
        phase_generation += 1;
    }
}
