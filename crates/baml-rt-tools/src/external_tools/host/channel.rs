// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
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
    protocol_v2::{CallContext, GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION},
};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use super::invalid;
use crate::mcp_snapshot::Digest;

type Reply = oneshot::Sender<std::result::Result<GuestMessage, String>>;

#[derive(Default)]
struct Pending {
    calls: HashMap<u64, Reply>,
    loads: HashMap<String, Reply>,
}

pub(super) struct SupervisorChannel {
    writer: AsyncMutex<FrameWriter>,
    pending: Arc<Mutex<Pending>>,
    loaded: AsyncMutex<HashSet<Digest>>,
    next_id: AtomicU64,
    healthy: Arc<AtomicBool>,
    reader: tokio::task::JoinHandle<()>,
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
                let mut pending = task_pending.lock().unwrap();
                let reply = match &message {
                    GuestMessage::Result { request_id, .. } => pending.calls.remove(request_id),
                    GuestMessage::LoadAck { tool_digest, .. } => pending.loads.remove(tool_digest),
                    GuestMessage::Event { .. } => continue,
                    GuestMessage::HelloAck { .. } => break,
                };
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(message));
                } else {
                    break;
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
        Ok(Self {
            writer: AsyncMutex::new(writer),
            pending,
            loaded: AsyncMutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
            healthy,
            reader: task,
            control_timeout: timeout,
        })
    }

    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub async fn load(&self, digest: Digest, image: Digest, bytes: &[u8]) -> Result<()> {
        let mut loaded = self.loaded.lock().await;
        if !self.healthy() {
            return Err(invalid("supervisor connection unavailable"));
        }
        if loaded.contains(&digest) {
            return Ok(());
        }
        let mut operation = OperationGuard {
            channel: self,
            complete: false,
        };
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .loads
            .insert(digest.to_string(), tx);
        let result = tokio::time::timeout(self.control_timeout, async {
            let mut writer = self.writer.lock().await;
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
            match rx
                .await
                .map_err(|_| invalid("supervisor load disconnected"))?
                .map_err(invalid)?
            {
                GuestMessage::LoadAck { ok: true, .. } => Ok(()),
                _ => Err(invalid("supervisor rejected tool bundle")),
            }
        })
        .await
        .map_err(|_| invalid("supervisor load timed out"))
        .and_then(|v| v);
        self.pending
            .lock()
            .unwrap()
            .loads
            .remove(&digest.to_string());
        if result.is_err() {
            self.close();
        } else {
            loaded.insert(digest);
        }
        operation.complete = true;
        result
    }

    pub async fn invoke(
        &self,
        digest: Digest,
        context: CallContext,
        input: serde_json::Value,
        timeout: Duration,
    ) -> Result<GuestMessage> {
        if !self.healthy() {
            return Err(invalid("supervisor connection unavailable"));
        }
        let mut operation = OperationGuard {
            channel: self,
            complete: false,
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().calls.insert(id, tx);
        let result = tokio::time::timeout(timeout, async {
            self.writer
                .lock()
                .await
                .send(
                    &serde_json::to_value(HostMessage::Invoke(Box::new(InvokeRequest {
                        request_id: id,
                        tool_digest: digest.to_string(),
                        deadline_ms: context.deadline_ms,
                        call_context: context,
                        input,
                    })))
                    .map_err(|e| invalid(e.to_string()))?,
                )
                .await
                .map_err(|e| invalid(e.to_string()))?;
            rx.await
                .map_err(|_| invalid("supervisor invoke disconnected"))?
                .map_err(invalid)
        })
        .await
        .map_err(|_| invalid("tool invocation timed out"))
        .and_then(|v| v);
        self.pending.lock().unwrap().calls.remove(&id);
        if result.is_err() {
            self.close();
        }
        operation.complete = true;
        result
    }

    pub fn close(&self) {
        self.healthy.store(false, Ordering::Release);
        self.reader.abort();
        let mut pending = self.pending.lock().unwrap();
        pending.calls.clear();
        pending.loads.clear();
    }
}

// Dropping a partly written frame or an unfinished load must retire the connection.
struct OperationGuard<'a> {
    channel: &'a SupervisorChannel,
    complete: bool,
}

impl Drop for OperationGuard<'_> {
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
