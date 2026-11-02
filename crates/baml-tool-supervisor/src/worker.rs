// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! One tool worker: a long-lived node process running the SDK `serve()` loop.
//!
//! The supervisor is the worker's "host": it writes `Invoke` frames to the
//! worker's stdin and reads `Event`/`Result` frames from its stdout. Deadline,
//! output cap, and cancel live here; the wire is the same one the vsock uses.

use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use baml_sandbox_protocol::{
    ErrorClass,
    codec::{FrameReader, FrameWriter},
    protocol_v2::{GuestMessage, HostMessage, InvokeRequest},
};
use tokio::{
    process::{Child, ChildStderr, ChildStdin, ChildStdout},
    sync::{Mutex as AsyncMutex, mpsc::UnboundedSender},
};

use crate::{
    limits::SupervisorLimits,
    sandbox::{REVEAL_FD_BASE, RevealFd, WorkerBackend, command_for},
    secrets::RevealMaterial,
    tool::LoadedTool,
    usage::{self, Usage},
};

static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

pub struct Worker {
    pub id: u64,
    pub tool_digest: String,
    pub pid: u32,
    io: AsyncMutex<WorkerIo>,
    process: StdMutex<Option<Child>>,
    logs: Arc<StdMutex<LogCapture>>,
    last_used: StdMutex<Instant>,
    /// Serialises calls on one warm worker; the SDK loop is sequential.
    pub call_lock: AsyncMutex<()>,
}

struct WorkerIo {
    stdin: FrameWriter,
    stdout: FrameReader,
}

struct LogCapture {
    cap: u64,
    captured: Vec<u8>,
    /// Total bytes seen, including discarded ones.
    total: u64,
    truncated: bool,
}

/// Outcome of one worker call. `result` is always a terminal `Result` frame.
pub struct CallOutcome {
    pub result: GuestMessage,
    pub usage: Usage,
    /// The worker must be dropped and reaped (death, protocol violation,
    /// deadline, or a per-call reveal worker at the end of its call).
    pub retire: bool,
    pub logs: String,
}

impl Worker {
    pub fn spawn(
        tool: &LoadedTool,
        backend: &dyn WorkerBackend,
        limits: &SupervisorLimits,
        reveals: &[RevealMaterial],
    ) -> std::io::Result<Arc<Self>> {
        let sandbox = tool.sandbox(limits);
        let fd_sources: Vec<RevealFd> = reveals
            .iter()
            .enumerate()
            .map(|(index, material)| RevealFd {
                name: material.name.clone(),
                source: material.source_fd(),
                target: REVEAL_FD_BASE + index as i32,
            })
            .collect();
        let command = command_for(
            "node",
            &tool.dir,
            limits.node_modules.as_deref(),
            &fd_sources,
            limits.worker_harness.as_deref(),
            Vec::new(),
        );
        let mut child = backend.spawn(&sandbox, command)?;
        let pid = child.id().unwrap_or_default();
        let stdin = child.stdin.take().expect("worker stdin is piped");
        let stdout = child.stdout.take().expect("worker stdout is piped");
        let stderr = child.stderr.take().expect("worker stderr is piped");

        let logs = Arc::new(StdMutex::new(LogCapture {
            cap: limits.default_output_cap,
            captured: Vec::new(),
            total: 0,
            truncated: false,
        }));
        spawn_log_pump(stderr, logs.clone());

        Ok(Arc::new(Self {
            id: NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed),
            tool_digest: tool.digest.clone(),
            pid,
            io: AsyncMutex::new(WorkerIo {
                stdin: FrameWriter::new(stdin),
                stdout: FrameReader::new(stdout),
            }),
            process: StdMutex::new(Some(child)),
            logs,
            last_used: StdMutex::new(Instant::now()),
            call_lock: AsyncMutex::new(()),
        }))
    }

    pub fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    pub fn is_alive(&self) -> bool {
        let mut process = self.process.lock().unwrap();
        match process.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Kill the worker's process group. Safe to call from the cancel path.
    pub fn signal_kill(&self) {
        if self.pid == 0 {
            return;
        }
        let pid = self.pid as i32;
        // SAFETY: negative pid targets the process group; ESRCH is harmless.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }

    pub async fn reap(&self) {
        let child = self.process.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }

    /// Run one invoke on this worker. The caller must hold `call_lock` and have
    /// recorded the worker's pid in the inflight entry for cancellation.
    pub async fn execute(
        &self,
        request: &InvokeRequest,
        deadline: Instant,
        max_result_bytes: usize,
        out: &UnboundedSender<GuestMessage>,
    ) -> CallOutcome {
        let started = Instant::now();
        let mut io = self.io.lock().await;
        let invoke = HostMessage::Invoke(Box::new(request.clone()));
        let frame = match serde_json::to_value(&invoke) {
            Ok(frame) => frame,
            Err(error) => {
                return self.failed(
                    request.request_id,
                    ErrorClass::Execution,
                    format!("cannot encode invoke: {error}"),
                    started,
                    true,
                );
            }
        };
        if let Err(error) = io.stdin.send(&frame).await {
            return self.failed(
                request.request_id,
                ErrorClass::Transient,
                format!("worker channel unavailable: {error}"),
                started,
                true,
            );
        }

        let deadline = tokio::time::Instant::from_std(deadline);
        loop {
            let received = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => {
                    self.signal_kill();
                    return self.failed(
                        request.request_id,
                        ErrorClass::Transient,
                        "deadline exceeded".to_string(),
                        started,
                        true,
                    );
                }
                received = io.stdout.recv() => received,
            };
            let value = match received {
                Ok(value) => value,
                Err(error) => {
                    return self.failed(
                        request.request_id,
                        ErrorClass::Transient,
                        format!("worker exited or sent a short frame: {error}"),
                        started,
                        true,
                    );
                }
            };
            let message: GuestMessage = match serde_json::from_value(value) {
                Ok(message) => message,
                Err(error) => {
                    self.signal_kill();
                    return self.failed(
                        request.request_id,
                        ErrorClass::Execution,
                        format!("worker sent a malformed frame: {error}"),
                        started,
                        true,
                    );
                }
            };
            match message {
                GuestMessage::Event {
                    request_id,
                    kind,
                    payload,
                } => {
                    // Forward progress/log events; a stale id is a protocol bug.
                    if request_id != request.request_id {
                        self.signal_kill();
                        return self.failed(
                            request.request_id,
                            ErrorClass::Execution,
                            "worker emitted an event for an unknown request".to_string(),
                            started,
                            true,
                        );
                    }
                    let _ = out.send(GuestMessage::Event {
                        request_id,
                        kind,
                        payload,
                    });
                }
                GuestMessage::Result {
                    request_id,
                    ok,
                    output,
                    error,
                    error_class,
                    truncated,
                } => {
                    if request_id != request.request_id {
                        self.signal_kill();
                        return self.failed(
                            request.request_id,
                            ErrorClass::Execution,
                            "worker emitted a result for an unknown request".to_string(),
                            started,
                            true,
                        );
                    }
                    let usage = usage::measure(self.pid, started.elapsed());
                    let logs = self.take_logs();
                    if logs.truncated {
                        // Per spec, an over-cap log stream is reported as a
                        // truncated result; the host treats it as an error.
                        return CallOutcome {
                            result: GuestMessage::Result {
                                request_id,
                                ok: false,
                                output,
                                error: Some(format!(
                                    "worker log exceeded {} bytes (saw {})",
                                    logs.cap, logs.total
                                )),
                                error_class: ErrorClass::Transient,
                                truncated: true,
                            },
                            usage,
                            // A flooding worker is killed, not reused.
                            retire: true,
                            logs: logs.text,
                        };
                    }
                    if let Some(size) = output
                        .as_ref()
                        .and_then(|value| serde_json::to_vec(value).ok().map(|bytes| bytes.len()))
                        && size > max_result_bytes
                    {
                        return CallOutcome {
                            result: GuestMessage::Result {
                                request_id,
                                ok: false,
                                output: None,
                                error: Some(format!(
                                    "worker result ({size} bytes) exceeds the {max_result_bytes}-byte cap"
                                )),
                                error_class: ErrorClass::Transient,
                                truncated: true,
                            },
                            usage,
                            // A flooding worker is killed, not reused.
                            retire: true,
                            logs: logs.text,
                        };
                    }
                    return CallOutcome {
                        result: GuestMessage::Result {
                            request_id,
                            ok,
                            output,
                            error,
                            error_class,
                            truncated,
                        },
                        usage,
                        retire: false,
                        logs: logs.text,
                    };
                }
                other => {
                    self.signal_kill();
                    return self.failed(
                        request.request_id,
                        ErrorClass::Execution,
                        format!("worker sent an unexpected message: {other:?}"),
                        started,
                        true,
                    );
                }
            }
        }
    }

    fn failed(
        &self,
        request_id: u64,
        error_class: ErrorClass,
        error: String,
        started: Instant,
        retire: bool,
    ) -> CallOutcome {
        let usage = usage::measure(self.pid, started.elapsed());
        let logs = self.take_logs();
        CallOutcome {
            result: GuestMessage::Result {
                request_id,
                ok: false,
                output: None,
                error: Some(error),
                error_class,
                truncated: false,
            },
            usage,
            retire,
            logs: logs.text,
        }
    }

    fn take_logs(&self) -> TakenLogs {
        let mut logs = self.logs.lock().unwrap();
        let taken = TakenLogs {
            cap: logs.cap,
            total: logs.total,
            truncated: logs.truncated,
            text: String::from_utf8_lossy(&logs.captured).into_owned(),
        };
        // A warm worker serves later calls: its log budget starts fresh.
        logs.captured.clear();
        logs.total = 0;
        logs.truncated = false;
        taken
    }
}

struct TakenLogs {
    cap: u64,
    total: u64,
    truncated: bool,
    text: String,
}

fn spawn_log_pump(mut stderr: ChildStderr, logs: Arc<StdMutex<LogCapture>>) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut chunk = [0_u8; 8192];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let mut logs = logs.lock().unwrap();
                    logs.total = logs.total.saturating_add(count as u64);
                    let remaining = logs.cap.saturating_sub(logs.captured.len() as u64) as usize;
                    if remaining == 0 {
                        logs.truncated = true;
                    } else {
                        let take = remaining.min(count);
                        logs.captured.extend_from_slice(&chunk[..take]);
                        if take < count {
                            logs.truncated = true;
                        }
                    }
                }
            }
        }
    });
}

// `FrameWriter` writes to `ChildStdin`; the type is only used through the
// `AsyncWrite` impl, so nothing else is required here.
const _: fn(ChildStdin) -> FrameWriter = FrameWriter::new;
const _: fn(ChildStdout) -> FrameReader = FrameReader::new;
