// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! The guest supervisor: one channel, one state machine, many tools.
//!
//! Framing note: the codec frames are untyped, so the reader is a state
//! machine. After a `LoadTool` header frame it reads exactly `bundle_len` raw
//! bytes before resuming JSON frames. Any decode error, unexpected frame, or
//! short/overlong bundle retires the connection, matching the host's
//! poisoned-connection policy.

use std::{
    collections::HashMap,
    io,
    os::unix::fs::DirBuilderExt as _,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use baml_sandbox_protocol::{
    ErrorClass,
    codec::{CodecError, FrameReader},
    protocol_v2::{GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION, RevealedSecret},
};
use tokio::{
    sync::{Mutex as AsyncMutex, mpsc},
    task::JoinHandle,
};

use crate::{
    error::SupervisorError,
    limits::SupervisorLimits,
    sandbox::WorkerBackend,
    secrets::{self, RevealMaterial},
    tool::LoadedTool,
    worker::Worker,
};

/// Version string reported in `HelloAck`.
pub const SUPERVISOR_NAME: &str = concat!("baml-tool-supervisor/", env!("CARGO_PKG_VERSION"));

/// Largest number of concurrently staged pre-invoke secrets.
const MAX_PENDING_REVEALS: usize = 1024;
/// Largest number of reveal secrets for one call.
const MAX_REVEALS_PER_CALL: usize = 16;

pub struct Supervisor {
    limits: SupervisorLimits,
    root: PathBuf,
    backend: Arc<dyn WorkerBackend>,
    state: Arc<State>,
}

struct State {
    outbound: mpsc::UnboundedSender<GuestMessage>,
    loaded: AsyncMutex<HashMap<String, Arc<LoadedTool>>>,
    workers: AsyncMutex<HashMap<String, Arc<Worker>>>,
    inflight: StdMutex<HashMap<u64, Arc<Inflight>>>,
    /// Secrets that arrived before their `Invoke` (the host stages reveal
    /// material first, then the invoke that consumes it).
    pending_reveals: StdMutex<HashMap<u64, Vec<RevealMaterial>>>,
    next_slot: AtomicU32,
    closed: AtomicBool,
}

struct Inflight {
    reveals: StdMutex<Vec<RevealMaterial>>,
    reveal_error: StdMutex<Option<String>>,
    worker_started: AtomicBool,
    /// Worker process group id, or 0 before spawn.
    pgid: AtomicI32,
    killed: AtomicBool,
}

impl Inflight {
    fn new() -> Self {
        Self {
            reveals: StdMutex::new(Vec::new()),
            reveal_error: StdMutex::new(None),
            worker_started: AtomicBool::new(false),
            pgid: AtomicI32::new(0),
            killed: AtomicBool::new(false),
        }
    }
}

impl State {
    fn send(&self, message: GuestMessage) {
        // A closed channel means the writer task retired the connection; the
        // reader will observe EOF and stop.
        let _ = self.outbound.send(message);
    }
}

impl Supervisor {
    pub fn new(
        limits: SupervisorLimits,
        root: PathBuf,
        backend: Arc<dyn WorkerBackend>,
        outbound: mpsc::UnboundedSender<GuestMessage>,
    ) -> io::Result<Arc<Self>> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "supervisor root must be absolute",
            ));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        // Inside the VM the supervisor is root: make the bundle root a real
        // tmpfs so unpacked tool bytes never touch the image layer.
        #[cfg(target_os = "linux")]
        if crate::sandbox::root::RootCloneBackend::running_as_root() {
            use nix::mount::{MsFlags, mount};
            let target = root.to_string_lossy().into_owned();
            mount(
                None::<&str>,
                target.as_str(),
                Some("tmpfs"),
                MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
                Some("size=512m,mode=0711"),
            )
            .map_err(|error| io::Error::other(format!("cannot mount the bundle tmpfs: {error}")))?;
        }
        Ok(Arc::new(Self {
            limits,
            root,
            backend,
            state: Arc::new(State {
                outbound,
                loaded: AsyncMutex::new(HashMap::new()),
                workers: AsyncMutex::new(HashMap::new()),
                inflight: StdMutex::new(HashMap::new()),
                pending_reveals: StdMutex::new(HashMap::new()),
                next_slot: AtomicU32::new(0),
                closed: AtomicBool::new(false),
            }),
        }))
    }

    pub fn limits(&self) -> &SupervisorLimits {
        &self.limits
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Serve until the channel closes or the peer violates the protocol.
    pub async fn run(self: Arc<Self>, mut reader: FrameReader) -> Result<(), SupervisorError> {
        let mut handshaken = false;
        loop {
            let value = match reader.recv().await {
                Ok(value) => value,
                Err(CodecError::Io { .. }) => break,
                Err(error) => return Err(SupervisorError::Protocol(error.to_string())),
            };
            let message: HostMessage = serde_json::from_value(value)
                .map_err(|error| SupervisorError::Protocol(error.to_string()))?;
            match message {
                HostMessage::Hello { proto, grant_class } => {
                    if handshaken {
                        return Err(SupervisorError::Protocol(
                            "duplicate hello frame".to_string(),
                        ));
                    }
                    if proto != PROTOCOL_V2_VERSION {
                        self.state.send(GuestMessage::Result {
                            request_id: 0,
                            ok: false,
                            output: None,
                            error: Some(format!(
                                "unsupported protocol version {proto}; supervisor speaks {PROTOCOL_V2_VERSION}"
                            )),
                            error_class: ErrorClass::Configuration,
                            truncated: false,
                        });
                        return Err(SupervisorError::Protocol(format!(
                            "protocol mismatch: peer sent {proto}"
                        )));
                    }
                    // Facts only: the grant class partitions VMs on the host,
                    // the guest gains no authority from it.
                    eprintln!(
                        "baml-tool-supervisor: hello from grant class {grant_class}, backend {}",
                        self.backend.name()
                    );
                    handshaken = true;
                    self.state.send(GuestMessage::HelloAck {
                        proto: PROTOCOL_V2_VERSION,
                        supervisor: SUPERVISOR_NAME.to_string(),
                    });
                }
                HostMessage::LoadTool {
                    tool_digest,
                    runtime_image_digest,
                    bundle_len,
                } => {
                    self.require_handshake(handshaken)?;
                    self.load(&mut reader, tool_digest, runtime_image_digest, bundle_len)
                        .await?;
                }
                HostMessage::Invoke(request) => {
                    self.require_handshake(handshaken)?;
                    let inflight = Arc::new(Inflight::new());
                    if let Some(reveals) = self
                        .state
                        .pending_reveals
                        .lock()
                        .unwrap()
                        .remove(&request.request_id)
                    {
                        *inflight.reveals.lock().unwrap() = reveals;
                    }
                    self.state
                        .inflight
                        .lock()
                        .unwrap()
                        .insert(request.request_id, inflight.clone());
                    let this = self.clone();
                    tokio::spawn(async move {
                        this.run_call(*request, inflight).await;
                    });
                }
                HostMessage::Cancel { request_id } => {
                    self.require_handshake(handshaken)?;
                    self.cancel(request_id);
                }
                HostMessage::Secret {
                    request_id,
                    name,
                    value,
                } => {
                    self.require_handshake(handshaken)?;
                    self.stage_secret(request_id, name, value);
                }
            }
        }
        Ok(())
    }

    fn require_handshake(&self, handshaken: bool) -> Result<(), SupervisorError> {
        if handshaken {
            Ok(())
        } else {
            Err(SupervisorError::Protocol(
                "message before hello handshake".to_string(),
            ))
        }
    }

    async fn load(
        &self,
        reader: &mut FrameReader,
        tool_digest: String,
        runtime_image_digest: String,
        bundle_len: u64,
    ) -> Result<(), SupervisorError> {
        if bundle_len > self.limits.max_bundle_bytes {
            return Err(SupervisorError::Protocol(format!(
                "bundle of {bundle_len} bytes exceeds the {} byte cap",
                self.limits.max_bundle_bytes
            )));
        }
        // Validate announced identities before they reach any path join.
        tool_digest
            .parse::<baml_ktool::Digest>()
            .map_err(|_| SupervisorError::Protocol("invalid tool digest".to_string()))?;
        runtime_image_digest
            .parse::<baml_ktool::Digest>()
            .map_err(|_| SupervisorError::Protocol("invalid runtime image digest".to_string()))?;

        let mut bytes = Vec::with_capacity((bundle_len as usize).min(1024 * 1024));
        while (bytes.len() as u64) < bundle_len {
            let chunk = reader
                .recv_bytes()
                .await
                .map_err(|error| SupervisorError::Protocol(error.to_string()))?;
            let remaining = bundle_len - bytes.len() as u64;
            if chunk.len() as u64 > remaining {
                return Err(SupervisorError::Protocol(
                    "bundle chunk overruns the announced length".to_string(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }

        if self.state.loaded.lock().await.contains_key(&tool_digest) {
            self.state.send(GuestMessage::LoadAck {
                tool_digest,
                ok: true,
                error: None,
            });
            return Ok(());
        }

        let slot = self.state.next_slot.fetch_add(1, Ordering::Relaxed);
        if slot >= 10_000 {
            self.state.send(GuestMessage::LoadAck {
                tool_digest,
                ok: false,
                error: Some("worker slot budget exhausted".to_string()),
            });
            return Ok(());
        }
        let uid = self.limits.uid_base.saturating_add(slot);
        let gid = self.limits.gid_base.saturating_add(slot);

        let root = self.root.clone();
        let limits = self.limits.clone();
        let digest = tool_digest.clone();
        let image = runtime_image_digest.clone();
        let unpacked = tokio::task::spawn_blocking(move || {
            let dest = root.join(digest.trim_start_matches("sha256:"));
            let unpacked = baml_ktool::unpack_ktool_bundle(
                &bytes,
                &digest,
                &image,
                &dest,
                limits.max_unpacked_bytes,
            )?;
            // One uid owns one bundle tree. The tmpfs root stays traversable
            // (0711) but other tools cannot read this directory.
            #[cfg(target_os = "linux")]
            if crate::sandbox::root::RootCloneBackend::running_as_root()
                && let Err(error) = crate::sandbox::root::seal_bundle(&unpacked.dir, uid, gid)
            {
                let _ = std::fs::remove_dir_all(&unpacked.dir);
                return Err(baml_ktool::KtoolError::Io(error));
            }
            Ok::<_, baml_ktool::KtoolError>(unpacked)
        })
        .await;

        match unpacked {
            Ok(Ok(unpacked)) => {
                let tool = Arc::new(LoadedTool::from_unpacked(unpacked, uid, gid, &self.limits));
                self.state
                    .loaded
                    .lock()
                    .await
                    .insert(tool_digest.clone(), tool);
                self.state.send(GuestMessage::LoadAck {
                    tool_digest,
                    ok: true,
                    error: None,
                });
            }
            Ok(Err(error)) => {
                self.state.send(GuestMessage::LoadAck {
                    tool_digest,
                    ok: false,
                    error: Some(error.to_string()),
                });
            }
            Err(error) => {
                self.state.send(GuestMessage::LoadAck {
                    tool_digest,
                    ok: false,
                    error: Some(format!("bundle unpack task failed: {error}")),
                });
            }
        }
        Ok(())
    }

    fn stage_secret(&self, request_id: u64, name: String, value: RevealedSecret) {
        let material = match secrets::create(name, &value.0) {
            Ok(material) => material,
            Err(error) => {
                self.fail_reveal(request_id, format!("cannot stage reveal secret: {error}"));
                return;
            }
        };
        let inflight = self
            .state
            .inflight
            .lock()
            .unwrap()
            .get(&request_id)
            .cloned();
        match inflight {
            Some(inflight) => {
                if inflight.worker_started.load(Ordering::Acquire) {
                    // The fd set is fixed at spawn; a late secret fails closed.
                    self.fail_reveal(
                        request_id,
                        "reveal secret arrived after the worker started".to_string(),
                    );
                } else {
                    let mut reveals = inflight.reveals.lock().unwrap();
                    if reveals.len() >= MAX_REVEALS_PER_CALL {
                        drop(reveals);
                        self.fail_reveal(request_id, "too many reveal secrets".to_string());
                    } else {
                        reveals.push(material);
                    }
                }
            }
            None => {
                let mut pending = self.state.pending_reveals.lock().unwrap();
                if pending.len() >= MAX_PENDING_REVEALS && !pending.contains_key(&request_id) {
                    return;
                }
                let staged = pending.entry(request_id).or_default();
                if staged.len() >= MAX_REVEALS_PER_CALL {
                    return;
                }
                staged.push(material);
            }
        }
    }

    fn fail_reveal(&self, request_id: u64, message: String) {
        if let Some(inflight) = self.state.inflight.lock().unwrap().get(&request_id) {
            *inflight.reveal_error.lock().unwrap() = Some(message);
        }
    }

    fn cancel(&self, request_id: u64) {
        let inflight = self
            .state
            .inflight
            .lock()
            .unwrap()
            .get(&request_id)
            .cloned();
        let Some(inflight) = inflight else {
            return;
        };
        inflight.killed.store(true, Ordering::Release);
        let pgid = inflight.pgid.load(Ordering::Acquire);
        if pgid > 0 {
            // SAFETY: the negative pid targets the process group. The direct
            // signal also kills the root-backend shim when the PID-namespace
            // worker cannot join that group. Its parent-death signal then
            // kills the worker too. ESRCH is benign for both calls.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
                libc::kill(pgid, libc::SIGKILL);
            }
        }
    }

    async fn run_call(self: Arc<Self>, request: InvokeRequest, inflight: Arc<Inflight>) {
        let request_id = request.request_id;
        let (mut result, usage) = self.execute_call(&request, &inflight).await;
        if inflight.killed.load(Ordering::Acquire) {
            result = GuestMessage::Result {
                request_id,
                ok: false,
                output: None,
                error: Some("invocation cancelled".to_string()),
                error_class: ErrorClass::Transient,
                truncated: false,
            };
        }
        if let Some(usage) = usage {
            self.state.send(GuestMessage::Event {
                request_id,
                kind: "usage".to_string(),
                payload: usage.payload(),
            });
        }
        self.state.send(result);
        self.state.inflight.lock().unwrap().remove(&request_id);
        self.state
            .pending_reveals
            .lock()
            .unwrap()
            .remove(&request_id);
    }

    async fn execute_call(
        &self,
        request: &InvokeRequest,
        inflight: &Arc<Inflight>,
    ) -> (GuestMessage, Option<crate::usage::Usage>) {
        let request_id = request.request_id;
        if inflight.killed.load(Ordering::Acquire) {
            return (
                error_result(
                    request_id,
                    ErrorClass::Transient,
                    "invocation cancelled".to_string(),
                ),
                None,
            );
        }
        if let Some(error) = inflight.reveal_error.lock().unwrap().take() {
            return (
                error_result(request_id, ErrorClass::Configuration, error),
                None,
            );
        }
        let deadline = absolute_deadline(request.deadline_ms, self.limits.max_call_ms);
        if deadline <= Instant::now() {
            return (
                error_result(
                    request_id,
                    ErrorClass::Transient,
                    "deadline elapsed before the call started".to_string(),
                ),
                None,
            );
        }

        let tool = self
            .state
            .loaded
            .lock()
            .await
            .get(&request.tool_digest)
            .cloned();
        let Some(tool) = tool else {
            return (
                error_result(
                    request_id,
                    ErrorClass::Configuration,
                    format!("tool {} is not loaded", request.tool_digest),
                ),
                None,
            );
        };

        let reveals = std::mem::take(&mut *inflight.reveals.lock().unwrap());
        let fresh = !reveals.is_empty();
        // The fd set is fixed at spawn. From here a late Secret must fail
        // closed instead of being silently dropped.
        inflight.worker_started.store(true, Ordering::Release);
        let worker = match self.acquire_worker(&tool, fresh, &reveals).await {
            Ok(worker) => worker,
            Err(error) => {
                return (
                    error_result(
                        request_id,
                        ErrorClass::Configuration,
                        format!("cannot start worker: {error}"),
                    ),
                    None,
                );
            }
        };

        let late_reveal_error = inflight.reveal_error.lock().unwrap().take();
        if let Some(error) = late_reveal_error {
            if fresh {
                self.retire_worker(&worker, fresh).await;
            }
            return (
                error_result(request_id, ErrorClass::Configuration, error),
                None,
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let guard = match tokio::time::timeout(remaining, worker.call_lock.lock()).await {
            Ok(guard) => guard,
            Err(_) => {
                // Never kill a worker that is busy with someone else's call.
                // A per-call reveal worker has no other call, so retire it.
                if fresh {
                    self.retire_worker(&worker, fresh).await;
                }
                return (
                    error_result(
                        request_id,
                        ErrorClass::Transient,
                        "deadline exceeded while waiting for the worker".to_string(),
                    ),
                    None,
                );
            }
        };
        inflight.pgid.store(worker.pid as i32, Ordering::Release);
        if inflight.killed.load(Ordering::Acquire) {
            drop(guard);
            if fresh {
                self.retire_worker(&worker, fresh).await;
            }
            return (
                error_result(
                    request_id,
                    ErrorClass::Transient,
                    "invocation cancelled".to_string(),
                ),
                None,
            );
        }
        let outcome = worker
            .execute(
                request,
                deadline,
                self.limits.max_result_bytes,
                &self.state.outbound,
            )
            .await;
        drop(guard);

        if outcome.retire || fresh {
            self.retire_worker(&worker, fresh).await;
        }
        if !outcome.logs.is_empty() {
            let mut logs = outcome.logs;
            logs.truncate(2048);
            eprintln!("baml-tool-supervisor: worker logs: {logs}");
        }
        (outcome.result, Some(outcome.usage))
    }

    async fn acquire_worker(
        &self,
        tool: &Arc<LoadedTool>,
        fresh: bool,
        reveals: &[RevealMaterial],
    ) -> io::Result<Arc<Worker>> {
        if fresh {
            // Reveal secrets are scoped to one call: never a warm worker.
            return Worker::spawn(tool, self.backend.as_ref(), &self.limits, reveals);
        }
        let mut workers = self.state.workers.lock().await;
        if let Some(worker) = workers.get(&tool.digest).cloned() {
            if worker.is_alive() {
                worker.touch();
                return Ok(worker);
            }
            workers.remove(&tool.digest);
            tokio::spawn(async move { worker.reap().await });
        }
        // Bound warm workers; evict the least-recently-used idle one.
        while workers.len() >= self.limits.max_warm_workers {
            let victim = workers
                .iter()
                .filter(|(_, worker)| Arc::strong_count(worker) == 1)
                .min_by_key(|(_, worker)| worker.idle_for())
                .map(|(digest, _)| digest.clone());
            let Some(victim) = victim else {
                return Err(io::Error::other(
                    "warm worker budget exhausted; all workers are busy",
                ));
            };
            if let Some(evicted) = workers.remove(&victim) {
                evicted.signal_kill();
                tokio::spawn(async move { evicted.reap().await });
            }
        }
        let worker = Worker::spawn(tool, self.backend.as_ref(), &self.limits, &[])?;
        workers.insert(tool.digest.clone(), worker.clone());
        Ok(worker)
    }

    async fn retire_worker(&self, worker: &Arc<Worker>, fresh: bool) {
        worker.signal_kill();
        worker.reap().await;
        if !fresh {
            let mut workers = self.state.workers.lock().await;
            if workers
                .get(&worker.tool_digest)
                .is_some_and(|current| current.id == worker.id)
            {
                workers.remove(&worker.tool_digest);
            }
        }
    }

    /// Kill idle warm workers. Only workers with no in-flight call (no extra
    /// `Arc`) are eligible.
    pub fn spawn_idle_reaper(self: &Arc<Self>) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(supervisor) = weak.upgrade() else {
                    break;
                };
                if supervisor.state.closed.load(Ordering::Acquire) {
                    break;
                }
                supervisor.reap_idle().await;
            }
        })
    }

    async fn reap_idle(&self) {
        let mut expired = Vec::new();
        {
            let mut workers = self.state.workers.lock().await;
            let digests: Vec<String> = workers
                .iter()
                .filter(|(_, worker)| {
                    Arc::strong_count(worker) == 1
                        && worker.idle_for() >= self.limits.worker_idle_timeout
                })
                .map(|(digest, _)| digest.clone())
                .collect();
            for digest in digests {
                if let Some(worker) = workers.remove(&digest) {
                    expired.push(worker);
                }
            }
        }
        for worker in expired {
            worker.signal_kill();
            worker.reap().await;
        }
    }

    /// As PID 1 in the VM, orphaned worker descendants are reparented here.
    /// Reap them so a killed process group cannot accumulate zombies.
    /// `try_wait`-style reaping of tracked children is unaffected: a stolen
    /// status only makes the worker look dead, which is what we want.
    pub fn spawn_orphan_reaper(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        // SAFETY: getpid has no preconditions.
        if unsafe { libc::getpid() } != 1 {
            return None;
        }
        let weak = Arc::downgrade(self);
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if weak.upgrade().is_none() {
                    break;
                }
                loop {
                    let mut status = 0;
                    // SAFETY: waitpid with WNOHANG does not block.
                    let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                    if reaped <= 0 {
                        break;
                    }
                }
            }
        }))
    }

    pub async fn shutdown(&self) {
        self.state.closed.store(true, Ordering::Release);
        let workers: Vec<_> = self
            .state
            .workers
            .lock()
            .await
            .drain()
            .map(|(_, w)| w)
            .collect();
        for worker in workers {
            worker.signal_kill();
            worker.reap().await;
        }
        self.state.inflight.lock().unwrap().clear();
        self.state.pending_reveals.lock().unwrap().clear();
    }
}

fn absolute_deadline(deadline_ms: u64, max_call_ms: u64) -> Instant {
    let now = Instant::now();
    if deadline_ms == 0 {
        return now + Duration::from_millis(max_call_ms);
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let capped = deadline_ms.min(now_ms.saturating_add(max_call_ms));
    if capped <= now_ms {
        now
    } else {
        now + Duration::from_millis(capped - now_ms)
    }
}

fn error_result(request_id: u64, class: ErrorClass, message: String) -> GuestMessage {
    GuestMessage::Result {
        request_id,
        ok: false,
        output: None,
        error: Some(message),
        error_class: class,
        truncated: false,
    }
}
