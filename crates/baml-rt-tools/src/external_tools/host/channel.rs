// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Multiplexed v2 RPC over one provider-owned vsock stream.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use baml_rt_core::Result;
use baml_sandbox_protocol::{
    TsrpcChannel,
    codec::FrameWriter,
    protocol_v2::{
        CallContext, GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION, RevealedSecret,
    },
};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    task::AbortHandle,
};

use super::{grants::RevealSecret, invalid};
use crate::mcp_snapshot::Digest;

type Reply = oneshot::Sender<std::result::Result<GuestMessage, String>>;

#[derive(Default)]
struct Pending {
    calls: HashMap<u64, Reply>,
    loads: HashMap<String, Reply>,
}

#[derive(Clone)]
struct ChannelControl {
    writer: Arc<AsyncMutex<FrameWriter>>,
    pending: Arc<Mutex<Pending>>,
    healthy: Arc<AtomicBool>,
    reader_abort: AbortHandle,
}

impl ChannelControl {
    fn remove_call(&self, request_id: u64) {
        self.pending.lock().unwrap().calls.remove(&request_id);
    }

    fn retire(&self) {
        self.healthy.store(false, Ordering::Release);
        self.reader_abort.abort();
        let mut pending = self.pending.lock().unwrap();
        for (_, reply) in pending.calls.drain() {
            let _ = reply.send(Err("supervisor connection closed".into()));
        }
        for (_, reply) in pending.loads.drain() {
            let _ = reply.send(Err("supervisor connection closed".into()));
        }
    }

    fn cancel_call(&self, request_id: u64) {
        self.remove_call(request_id);
        if !self.healthy.load(Ordering::Acquire) {
            return;
        }
        let control = self.clone();
        let task = async move {
            if !control.healthy.load(Ordering::Acquire) {
                return;
            }
            let value = serde_json::to_value(HostMessage::Cancel { request_id })
                .expect("cancel message serializes");
            let failed = {
                let mut writer = control.writer.lock().await;
                if !control.healthy.load(Ordering::Acquire) {
                    false
                } else {
                    writer.send(&value).await.is_err()
                }
            };
            if failed {
                control.retire();
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(task);
            }
            Err(_) => self.retire(),
        }
    }
}

pub(super) struct SupervisorChannel {
    control: ChannelControl,
    loaded: AsyncMutex<HashSet<Digest>>,
    next_id: AtomicU64,
    _reader: tokio::task::JoinHandle<()>,
    control_timeout: Duration,
}

impl SupervisorChannel {
    pub async fn connect(
        mut channel: TsrpcChannel,
        grant_class: Digest,
        timeout: Duration,
    ) -> Result<Self> {
        tokio::time::timeout(timeout, async {
            channel
                .send(
                    &serde_json::to_value(HostMessage::Hello {
                        proto: PROTOCOL_V2_VERSION,
                        grant_class: grant_class.to_string(),
                    })
                    .map_err(|e| invalid(e.to_string()))?,
                )
                .await
                .map_err(|e| invalid(e.to_string()))?;
            match serde_json::from_value::<GuestMessage>(
                channel.recv().await.map_err(|e| invalid(e.to_string()))?,
            ) {
                Ok(GuestMessage::HelloAck {
                    proto: PROTOCOL_V2_VERSION,
                    ..
                }) => Ok(()),
                _ => Err(invalid("supervisor protocol handshake mismatch")),
            }
        })
        .await
        .map_err(|_| invalid("supervisor handshake timed out"))??;
        let (mut reader, writer) = channel.into_split();
        let writer = Arc::new(AsyncMutex::new(writer));
        let pending = Arc::new(Mutex::new(Pending::default()));
        let healthy = Arc::new(AtomicBool::new(true));
        let task_pending = pending.clone();
        let task_healthy = healthy.clone();
        let task = tokio::spawn(async move {
            loop {
                let message = match reader.recv().await {
                    Ok(value) => match serde_json::from_value::<GuestMessage>(value) {
                        Ok(message) => message,
                        Err(_) => break,
                    },
                    Err(_) => break,
                };
                let reply = {
                    let mut pending = task_pending.lock().unwrap();
                    match &message {
                        GuestMessage::Result { request_id, .. } => pending.calls.remove(request_id),
                        GuestMessage::LoadAck { tool_digest, .. } => {
                            pending.loads.remove(tool_digest)
                        }
                        // Events are best effort. The host currently does not
                        // expose a per-call event stream; it keeps the
                        // supervisor's per-call resource accounting as metrics.
                        GuestMessage::Event { kind, payload, .. } => {
                            if kind == "usage" {
                                let field = |name: &str| {
                                    payload.get(name).and_then(serde_json::Value::as_u64)
                                };
                                if let (Some(cpu_ms), Some(peak_rss_kb)) =
                                    (field("cpu_ms"), field("peak_rss_kb"))
                                {
                                    crate::metrics::record_tool_call_usage(cpu_ms, peak_rss_kb);
                                }
                            }
                            None
                        }
                        GuestMessage::HelloAck { .. } => break,
                    }
                };
                // A result for a timed-out or cancelled call is expected. It
                // must not poison the shared connection or other calls.
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(message));
                }
            }
            task_healthy.store(false, Ordering::Release);
            let mut pending = task_pending.lock().unwrap();
            for (_, reply) in pending.calls.drain() {
                let _ = reply.send(Err("supervisor connection closed".into()));
            }
            for (_, reply) in pending.loads.drain() {
                let _ = reply.send(Err("supervisor connection closed".into()));
            }
        });
        let control = ChannelControl {
            writer,
            pending,
            healthy,
            reader_abort: task.abort_handle(),
        };
        Ok(Self {
            control,
            loaded: AsyncMutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
            _reader: task,
            control_timeout: timeout,
        })
    }

    pub fn healthy(&self) -> bool {
        self.control.healthy.load(Ordering::Acquire)
    }

    pub async fn load(&self, digest: Digest, image: Digest, bytes: &[u8]) -> Result<()> {
        let mut loaded = self.loaded.lock().await;
        if !self.healthy() {
            return Err(invalid("supervisor connection unavailable"));
        }
        if loaded.contains(&digest) {
            return Ok(());
        }
        let mut operation = LoadGuard {
            channel: self,
            complete: false,
        };
        let (tx, rx) = oneshot::channel();
        self.control
            .pending
            .lock()
            .unwrap()
            .loads
            .insert(digest.to_string(), tx);
        let response = tokio::time::timeout(self.control_timeout, async {
            let mut writer = self.control.writer.lock().await;
            writer
                .send(
                    &serde_json::to_value(HostMessage::LoadTool {
                        tool_digest: digest.to_string(),
                        runtime_image_digest: image.to_string(),
                        bundle_len: bytes.len() as u64,
                    })
                    .map_err(|e| invalid(e.to_string()))?,
                )
                .await
                .map_err(|e| invalid(e.to_string()))?;
            for chunk in bytes.chunks(64 * 1024) {
                writer
                    .send_bytes(chunk)
                    .await
                    .map_err(|e| invalid(e.to_string()))?;
            }
            drop(writer);
            rx.await
                .map_err(|_| invalid("supervisor load disconnected"))?
                .map_err(invalid)
        })
        .await;
        self.control
            .pending
            .lock()
            .unwrap()
            .loads
            .remove(&digest.to_string());
        let result = match response {
            Err(_) => {
                self.close();
                Err(invalid("supervisor load timed out"))
            }
            Ok(Err(error)) => {
                self.close();
                Err(error)
            }
            Ok(Ok(GuestMessage::LoadAck { ok: true, .. })) => {
                loaded.insert(digest);
                Ok(())
            }
            Ok(Ok(GuestMessage::LoadAck {
                ok: false, error, ..
            })) => {
                Err(invalid(error.unwrap_or_else(|| {
                    "supervisor rejected tool bundle".into()
                })))
            }
            Ok(Ok(_)) => {
                self.close();
                Err(invalid("unexpected supervisor load response"))
            }
        };
        operation.complete = true;
        result
    }

    pub async fn invoke(
        &self,
        digest: Digest,
        tool: String,
        context: CallContext,
        input: serde_json::Value,
        timeout: Duration,
        reveals: &[RevealSecret],
    ) -> Result<GuestMessage> {
        if !self.healthy() {
            return Err(invalid("supervisor connection unavailable"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.control.pending.lock().unwrap().calls.insert(id, tx);
        let mut operation = InvokeGuard::new(self.control.clone(), id);
        let response = tokio::time::timeout(timeout, async {
            // Reveal secrets are staged for this exact request id *before* the
            // invoke that consumes them; the supervisor keys them by request
            // id and never puts the value in the invoke payload.
            let mut writer = self.control.writer.lock().await;
            operation.mark_writing();
            for reveal in reveals {
                writer
                    .send(
                        &serde_json::to_value(HostMessage::Secret {
                            request_id: id,
                            name: reveal.name.clone(),
                            value: RevealedSecret(reveal.value.as_str().to_string()),
                        })
                        .map_err(|e| invalid(e.to_string()))?,
                    )
                    .await
                    .map_err(|e| invalid(e.to_string()))?;
            }
            writer
                .send(
                    &serde_json::to_value(HostMessage::Invoke(Box::new(InvokeRequest {
                        request_id: id,
                        tool,
                        tool_digest: digest.to_string(),
                        deadline_ms: context.deadline_ms,
                        call_context: context,
                        input,
                    })))
                    .map_err(|e| invalid(e.to_string()))?,
                )
                .await
                .map_err(|e| invalid(e.to_string()))?;
            operation.mark_sent();
            drop(writer);
            rx.await
                .map_err(|_| invalid("supervisor invoke disconnected"))?
                .map_err(invalid)
        })
        .await;
        let result = match response {
            Err(_) => Err(invalid("tool invocation timed out")),
            Ok(result) => result,
        };
        if result.is_ok() {
            self.control.remove_call(id);
            operation.finish();
        } else {
            operation.abort();
        }
        result
    }

    pub fn close(&self) {
        self.control.retire();
    }
}

#[derive(Clone, Copy)]
enum InvokePhase {
    Pending,
    Writing,
    Sent,
    Complete,
}

struct InvokeGuard {
    control: ChannelControl,
    request_id: u64,
    phase: InvokePhase,
}

impl InvokeGuard {
    fn new(control: ChannelControl, request_id: u64) -> Self {
        Self {
            control,
            request_id,
            phase: InvokePhase::Pending,
        }
    }

    fn mark_writing(&mut self) {
        self.phase = InvokePhase::Writing;
    }

    fn mark_sent(&mut self) {
        self.phase = InvokePhase::Sent;
    }

    fn finish(&mut self) {
        self.phase = InvokePhase::Complete;
    }

    fn abort(&mut self) {
        let phase = std::mem::replace(&mut self.phase, InvokePhase::Complete);
        match phase {
            InvokePhase::Pending => self.control.remove_call(self.request_id),
            InvokePhase::Writing => {
                self.control.remove_call(self.request_id);
                // A dropped write may have left a partial frame on the stream.
                self.control.retire();
            }
            InvokePhase::Sent => self.control.cancel_call(self.request_id),
            InvokePhase::Complete => {}
        }
    }
}

impl Drop for InvokeGuard {
    fn drop(&mut self) {
        if !matches!(self.phase, InvokePhase::Complete) {
            self.abort();
        }
    }
}

// Dropping an unfinished load can leave the peer in its raw-bundle state, so
// the connection must be retired rather than reused.
struct LoadGuard<'a> {
    channel: &'a SupervisorChannel,
    complete: bool,
}

impl Drop for LoadGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.channel.close();
        }
    }
}

impl Drop for SupervisorChannel {
    fn drop(&mut self) {
        self.close();
    }
}
