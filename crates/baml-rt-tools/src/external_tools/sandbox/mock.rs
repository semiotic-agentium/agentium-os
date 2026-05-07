//! In-memory [`SandboxProvider`] for tests and fixture-driven dev runs.
//!
//! No VM. Each `create` allocates a duplex pair: one end becomes the runtime
//! TSRPC channel, the other end is handed to a caller-supplied scripted
//! responder that emulates the guest-side `tool-adapter`. Lets us exercise
//! the sandbox invoker + cache + dispatch paths without pulling
//! microsandbox in.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use futures_util::stream::{self, BoxStream};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

use super::{
    channel::TsrpcChannel,
    provider::SandboxProvider,
    spec::{SandboxEvent, SandboxHandle, SandboxSpec},
};

/// Function that emulates a guest adapter: receives frames from the runtime
/// over `guest` and writes responses back.
pub type ScriptedAdapter =
    Arc<dyn Fn(DuplexStream) -> tokio::task::JoinHandle<()> + Send + Sync + 'static>;

/// In-memory provider for tests.
#[derive(Clone)]
pub struct MockSandboxProvider {
    inner: Arc<Mutex<MockInner>>,
    adapter: ScriptedAdapter,
}

struct MockInner {
    sandboxes: HashMap<String, MockSandboxState>,
}

struct MockSandboxState {
    handle: SandboxHandle,
    /// Set `Some` when the sandbox is considered terminated. `teardown` flips
    /// it to `Some("teardown")`.
    terminated: Option<String>,
}

impl MockSandboxProvider {
    /// Build a mock that uses `adapter` to drive guest-side replies on each
    /// `rpc_channel` call.
    pub fn new(adapter: ScriptedAdapter) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockInner {
                sandboxes: HashMap::new(),
            })),
            adapter,
        }
    }

    /// Convenience: echo-style adapter that replies to every frame with
    /// `{"echo": <input>}`. Enough for the smoke test in B7.
    pub fn echo() -> Self {
        let adapter: ScriptedAdapter = Arc::new(|stream| {
            tokio::spawn(async move {
                let (mut r, mut w) = tokio::io::split(stream);
                loop {
                    let mut len_buf = [0u8; 4];
                    if r.read_exact(&mut len_buf).await.is_err() {
                        break;
                    }
                    let len = u32::from_be_bytes(len_buf) as usize;
                    let mut body = vec![0u8; len];
                    if r.read_exact(&mut body).await.is_err() {
                        break;
                    }
                    let inbound: Value = match serde_json::from_slice(&body) {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    let reply = serde_json::json!({ "echo": inbound });
                    let out = match serde_json::to_vec(&reply) {
                        Ok(b) => b,
                        Err(_) => break,
                    };
                    if w.write_all(&(out.len() as u32).to_be_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if w.write_all(&out).await.is_err() {
                        break;
                    }
                    if w.flush().await.is_err() {
                        break;
                    }
                }
            })
        });
        Self::new(adapter)
    }
}

#[async_trait]
impl SandboxProvider for MockSandboxProvider {
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle> {
        let mut handle = SandboxHandle::new(&spec.name, spec.max_duration);
        handle.guest_workdir = spec.guest_workdir.clone();
        handle.runtime_digest = spec.runtime_digest.clone();
        handle.policy_hash = spec.policy_hash.clone();
        let mut inner = self.inner.lock().unwrap();
        inner.sandboxes.insert(
            spec.name.clone(),
            MockSandboxState {
                handle: handle.clone(),
                terminated: None,
            },
        );
        Ok(handle)
    }

    async fn rpc_channel(&self, handle: &SandboxHandle) -> Result<TsrpcChannel> {
        {
            let inner = self.inner.lock().unwrap();
            let state = inner.sandboxes.get(&handle.name).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!("mock sandbox '{}' not found", handle.name))
            })?;
            if let Some(reason) = &state.terminated {
                return Err(BamlRtError::InvalidArgument(format!(
                    "mock sandbox '{}' terminated: {}",
                    handle.name, reason
                )));
            }
        }
        let (host, guest) = duplex(64 * 1024);
        (self.adapter)(guest);
        let (r, w) = tokio::io::split(host);
        Ok(TsrpcChannel::new(r, w))
    }

    async fn teardown(&self, handle: &SandboxHandle) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(state) = inner.sandboxes.get_mut(&handle.name) {
            state.terminated = Some("teardown".to_string());
        }
        Ok(())
    }

    fn events(&self, _handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
        Box::pin(stream::empty())
    }

    async fn list_owned(&self, runner_id: &str) -> Result<Vec<SandboxHandle>> {
        let prefix = format!("baml:{runner_id}:");
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .sandboxes
            .values()
            .filter(|s| s.handle.name.starts_with(&prefix))
            .map(|s| s.handle.clone())
            .collect())
    }

    async fn reattach(&self, name: &str) -> Result<SandboxHandle> {
        let inner = self.inner.lock().unwrap();
        inner
            .sandboxes
            .get(name)
            .filter(|s| s.terminated.is_none())
            .map(|s| s.handle.clone())
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!("mock sandbox '{name}' not reattachable"))
            })
    }
}

impl MockSandboxProvider {
    /// Test helper: mark a sandbox as terminated to simulate
    /// `SandboxTerminatedUnexpectedly`.
    pub fn force_terminate(&self, name: &str, reason: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(state) = inner.sandboxes.get_mut(name) {
            state.terminated = Some(reason.to_string());
        }
    }
}

/// Convenience: short idle/max durations for tests so age-check paths can
/// fire deterministically without sleeping.
pub fn test_durations() -> (Duration, Duration) {
    (Duration::from_secs(300), Duration::from_secs(3600))
}
