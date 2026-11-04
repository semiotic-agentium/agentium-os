// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! One long-lived MCP stdio worker.
//!
//! The supervisor owns one MCP connection per loaded toolset. A reader task
//! routes JSON-RPC responses by id, so concurrent `tools/call` requests do not
//! depend on response order. The worker process remains isolated by the
//! selected `WorkerBackend`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use baml_sandbox_protocol::{
    ErrorClass,
    protocol_v2::{GuestMessage, InvokeRequest},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout},
    sync::{Mutex as AsyncMutex, Semaphore, mpsc::UnboundedSender, oneshot},
    task::JoinHandle,
};

use crate::{
    limits::SupervisorLimits,
    sandbox::{REVEAL_FD_BASE, RevealFd, WorkerBackend, command_for_argv},
    secrets::RevealMaterial,
    tool::LoadedTool,
    usage::{self, Usage},
};

static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_MCP_ID: AtomicU64 = AtomicU64::new(1000);

struct EventRoute {
    request_id: u64,
    output: UnboundedSender<GuestMessage>,
}

type PendingReply = oneshot::Sender<Result<Value, String>>;

/// JSON-RPC transport for one child process. Only the writer is locked. The
/// reader is independent and dispatches replies to the pending request map.
struct McpConnection {
    writer: AsyncMutex<ChildStdin>,
    pending: Arc<StdMutex<HashMap<u64, PendingReply>>>,
    events: Arc<StdMutex<HashMap<u64, EventRoute>>>,
    closed: Arc<AtomicBool>,
    reader: StdMutex<Option<JoinHandle<()>>>,
}

impl McpConnection {
    fn new(stdin: ChildStdin, stdout: ChildStdout, max_line_bytes: usize) -> Arc<Self> {
        let pending = Arc::new(StdMutex::new(HashMap::new()));
        let events = Arc::new(StdMutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let connection = Arc::new(Self {
            writer: AsyncMutex::new(stdin),
            pending: pending.clone(),
            events: events.clone(),
            closed: closed.clone(),
            reader: StdMutex::new(None),
        });
        let reader = tokio::spawn(async move {
            read_loop(stdout, pending, events, closed, max_line_bytes).await;
        });
        *connection.reader.lock().unwrap() = Some(reader);
        connection
    }

    async fn request(&self, id: u64, message: Value, timeout: Duration) -> Result<Value, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("MCP worker connection is closed".into());
        }
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let write_result = {
            let mut writer = self.writer.lock().await;
            write_line(&mut writer, &message).await
        };
        if let Err(error) = write_result {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("MCP worker write failed: {error}"));
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("MCP worker disconnected".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err("MCP response timed out".into())
            }
        }
    }

    async fn request_with_events(
        &self,
        id: u64,
        host_request_id: u64,
        message: Value,
        timeout: Duration,
        output: UnboundedSender<GuestMessage>,
    ) -> Result<Value, String> {
        self.events.lock().unwrap().insert(
            id,
            EventRoute {
                request_id: host_request_id,
                output,
            },
        );
        let result = self.request(id, message, timeout).await;
        self.events.lock().unwrap().remove(&id);
        result
    }

    async fn notify(&self, method: &str, params: Value) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut writer = self.writer.lock().await;
        if write_line(&mut writer, &message).await.is_err() {
            self.closed.store(true, Ordering::Release);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for (_, sender) in pending {
            let _ = sender.send(Err("MCP worker connection closed".into()));
        }
        if let Some(reader) = self.reader.lock().unwrap().take() {
            reader.abort();
        }
    }
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Arc<StdMutex<HashMap<u64, PendingReply>>>,
    events: Arc<StdMutex<HashMap<u64, EventRoute>>>,
    closed: Arc<AtomicBool>,
    max_line_bytes: usize,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let read = match reader.read_line(&mut line).await {
            Ok(read) => read,
            Err(error) => {
                fail_pending(&pending, format!("MCP worker read failed: {error}"));
                break;
            }
        };
        if read == 0 {
            fail_pending(&pending, "MCP worker exited before responding".into());
            break;
        }
        if line.len() > max_line_bytes {
            fail_pending(
                &pending,
                format!("MCP response line exceeds the {max_line_bytes}-byte raw output cap"),
            );
            break;
        }
        let value: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(error) => {
                fail_pending(&pending, format!("MCP worker sent malformed JSON: {error}"));
                break;
            }
        };

        if value.get("id").is_none() {
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                continue;
            };
            if !matches!(method, "notifications/progress" | "notifications/message") {
                continue;
            }
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let mcp_id = params
                .get("progressToken")
                .and_then(Value::as_u64)
                .or_else(|| {
                    params
                        .get("_meta")
                        .and_then(|meta| meta.get("progressToken"))
                        .and_then(Value::as_u64)
                });
            let route = {
                let routes = events.lock().unwrap();
                mcp_id
                    .and_then(|id| routes.get(&id))
                    .or_else(|| {
                        (routes.len() == 1)
                            .then(|| routes.values().next())
                            .flatten()
                    })
                    .map(|route| (route.request_id, route.output.clone()))
            };
            if let Some((request_id, output)) = route {
                let _ = output.send(GuestMessage::Event {
                    request_id,
                    kind: method
                        .strip_prefix("notifications/")
                        .unwrap_or(method)
                        .to_string(),
                    payload: params,
                });
            }
            continue;
        }

        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(sender) = pending.lock().unwrap().remove(&id) else {
            continue;
        };
        let result = if let Some(error) = value.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP request failed");
            if code == -32602 {
                Err(format!("invalid argument: {message}"))
            } else {
                Err(format!("MCP JSON-RPC error {code}: {message}"))
            }
        } else {
            value
                .get("result")
                .cloned()
                .ok_or_else(|| "MCP response has no result".to_string())
        };
        let _ = sender.send(result);
    }
    closed.store(true, Ordering::Release);
    fail_pending(&pending, "MCP worker connection closed".into());
}

fn fail_pending(pending: &StdMutex<HashMap<u64, PendingReply>>, error: String) {
    let pending = std::mem::take(&mut *pending.lock().unwrap());
    for (_, sender) in pending {
        let _ = sender.send(Err(error.clone()));
    }
}

pub struct Worker {
    pub id: u64,
    pub tool_digest: String,
    pub pid: u32,
    connection: Arc<McpConnection>,
    process: StdMutex<Option<Child>>,
    logs: Arc<StdMutex<LogCapture>>,
    last_used: StdMutex<Instant>,
    active_calls: StdMutex<HashMap<u64, u64>>,
    inflight: Semaphore,
}

struct LogCapture {
    cap: u64,
    captured: Vec<u8>,
    total: u64,
    truncated: bool,
}

pub struct CallOutcome {
    pub result: GuestMessage,
    pub usage: Usage,
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
        let command = if limits.worker_harness.is_some() {
            crate::sandbox::command_for_harness(
                "node",
                &tool.dir,
                limits.node_modules.as_deref(),
                &fd_sources,
                limits.worker_harness.as_deref().expect("harness checked"),
                Vec::new(),
            )
        } else {
            command_for_argv(
                &tool.command,
                &tool.dir,
                limits.node_modules.as_deref(),
                &fd_sources,
                Vec::new(),
            )
        };
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
            connection: McpConnection::new(stdin, stdout, limits.max_result_bytes),
            process: StdMutex::new(Some(child)),
            logs,
            last_used: StdMutex::new(Instant::now()),
            active_calls: StdMutex::new(HashMap::new()),
            inflight: Semaphore::new(limits.max_inflight_per_worker.max(1)),
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

    pub fn is_healthy(&self) -> bool {
        self.is_alive() && !self.connection.closed.load(Ordering::Acquire)
    }

    pub fn signal_kill(&self) {
        if self.pid == 0 {
            return;
        }
        let pid = self.pid as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }

    pub async fn reap(&self) {
        self.connection.close();
        let child = self.process.lock().unwrap().take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }

    pub async fn cancel(&self, host_request_id: u64) {
        let Some(mcp_id) = self
            .active_calls
            .lock()
            .unwrap()
            .get(&host_request_id)
            .copied()
        else {
            return;
        };
        self.cancel_mcp(mcp_id).await;
    }

    async fn cancel_mcp(&self, mcp_id: u64) {
        self.connection
            .notify("notifications/cancelled", json!({"requestId": mcp_id}))
            .await;
    }

    pub async fn verify_tools(
        &self,
        expected: &[baml_ktool::ToolDescriptor],
        timeout: Duration,
    ) -> Result<(), String> {
        let init_id = NEXT_MCP_ID.fetch_add(1, Ordering::Relaxed);
        let init = json!({
            "jsonrpc": "2.0", "id": init_id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "baml-tool-supervisor", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        if let Err(error) = self.connection.request(init_id, init, timeout).await {
            return Err(format!("{error}; {}", self.diagnostic()));
        }
        self.connection
            .notify("notifications/initialized", json!({}))
            .await;
        let list_id = NEXT_MCP_ID.fetch_add(1, Ordering::Relaxed);
        let result = self
            .connection
            .request(
                list_id,
                json!({"jsonrpc": "2.0", "id": list_id, "method": "tools/list", "params": {}}),
                timeout,
            )
            .await
            .map_err(|error| format!("{error}; {}", self.diagnostic()))?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "tools/list result missing tools array".to_string())?;
        if tools.len() != expected.len() {
            return Err(format!(
                "tools/list returned {} tools, audited bundle has {}",
                tools.len(),
                expected.len()
            ));
        }
        for descriptor in expected {
            let local_name = descriptor
                .name
                .split_once('/')
                .map(|(_, local)| local)
                .unwrap_or(&descriptor.name);
            let live = tools
                .iter()
                .find(|tool| {
                    tool.get("name") == Some(&Value::String(local_name.to_string()))
                        || tool.get("name") == Some(&Value::String(descriptor.name.clone()))
                })
                .ok_or_else(|| format!("tools/list is missing {}", descriptor.name))?;
            let mut live = baml_ktool::ToolDescriptor::from_mcp(live)
                .map_err(|e| format!("invalid MCP descriptor for {}: {e}", descriptor.name))?;
            // The MCP worker reports only its local tool name; the bundle owns
            // the `toolset/` prefix. Normalize the name before the interface
            // digest comparison. Version needs no normalization: it is excluded
            // from `compute_descriptor_digest` (plan §16.1), so the live
            // descriptor's empty version cannot drift against the stamped one.
            live.name = descriptor.name.clone();
            if baml_ktool::compute_descriptor_digest(&live)
                != baml_ktool::compute_descriptor_digest(descriptor)
            {
                return Err(format!(
                    "tools/list descriptor drift for {}",
                    descriptor.name
                ));
            }
        }
        Ok(())
    }

    pub async fn execute(
        &self,
        request: &InvokeRequest,
        deadline: Instant,
        max_result_bytes: usize,
        out: &UnboundedSender<GuestMessage>,
    ) -> CallOutcome {
        let started = Instant::now();
        let timeout = deadline.saturating_duration_since(Instant::now());
        let permit = match tokio::time::timeout(timeout, self.inflight.acquire()).await {
            Ok(Ok(permit)) => permit,
            _ => {
                return self.failed(
                    request.request_id,
                    ErrorClass::Transient,
                    "deadline exceeded while waiting for the toolset concurrency slot".into(),
                    started,
                    false,
                );
            }
        };
        self.touch();
        let mcp_id = NEXT_MCP_ID.fetch_add(1, Ordering::Relaxed);
        let local_name = request
            .tool
            .split_once('/')
            .map(|(_, local)| local)
            .unwrap_or(&request.tool);
        self.active_calls
            .lock()
            .unwrap()
            .insert(request.request_id, mcp_id);
        let call = json!({
            "jsonrpc": "2.0", "id": mcp_id, "method": "tools/call",
            "params": {
                "name": local_name,
                "arguments": request.input.clone(),
                "_meta": {
                    "karakara/call_context": request.call_context.clone(),
                    "progressToken": mcp_id
                }
            }
        });
        let timeout = deadline.saturating_duration_since(Instant::now());
        let result = self
            .connection
            .request_with_events(mcp_id, request.request_id, call, timeout, out.clone())
            .await;
        self.active_calls
            .lock()
            .unwrap()
            .remove(&request.request_id);
        drop(permit);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if error == "MCP response timed out" {
                    self.cancel_mcp(mcp_id).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    self.signal_kill();
                    return self.failed(
                        request.request_id,
                        ErrorClass::Transient,
                        "deadline exceeded".into(),
                        started,
                        true,
                    );
                }
                let dead = !self.is_alive();
                let diagnostic = self.diagnostic();
                let error = format!("{error}; {diagnostic}");
                let retired = dead || self.connection.closed.load(Ordering::Acquire);
                let class = if error.starts_with("invalid argument:") {
                    ErrorClass::InvalidArgument
                } else if dead || self.connection.closed.load(Ordering::Acquire) {
                    ErrorClass::Transient
                } else {
                    ErrorClass::Execution
                };
                return self.failed(request.request_id, class, error, started, retired);
            }
        };
        let usage = usage::measure(self.pid, started.elapsed());
        let logs = self.take_logs();
        if logs.truncated {
            return self.failed_with_usage(
                request.request_id,
                ErrorClass::Transient,
                format!(
                    "worker log exceeded {} bytes (saw {})",
                    logs.cap, logs.total
                ),
                usage,
                logs.text,
                true,
            );
        }
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let output = result
            .get("structuredContent")
            .cloned()
            .or_else(|| {
                result
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|content| {
                        if content.len() == 1 {
                            content[0].get("text").and_then(Value::as_str).map(|text| {
                                serde_json::from_str(text)
                                    .unwrap_or_else(|_| Value::String(text.to_string()))
                            })
                        } else {
                            None
                        }
                    })
            })
            .unwrap_or_else(|| result.get("content").cloned().unwrap_or(Value::Null));
        let size = serde_json::to_vec(&output)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if size > max_result_bytes {
            return self.failed_with_usage(
                request.request_id,
                ErrorClass::Transient,
                format!("worker result ({size} bytes) exceeds the {max_result_bytes}-byte cap"),
                usage,
                logs.text,
                true,
            );
        }
        // The MCP TS SDK reports schema failures as an isError call result
        // whose text preserves JSON-RPC's -32602 code. Treat it the same as a
        // direct -32602 response so agents can correct their arguments.
        let error_class = if is_error
            && output
                .as_str()
                .is_some_and(|text| text.starts_with("MCP error -32602:"))
        {
            ErrorClass::InvalidArgument
        } else {
            ErrorClass::Execution
        };
        CallOutcome {
            result: GuestMessage::Result {
                request_id: request.request_id,
                ok: !is_error,
                output: (!is_error).then_some(output.clone()),
                error: is_error.then(|| output.to_string()),
                error_class,
                truncated: false,
            },
            usage,
            retire: false,
            logs: logs.text,
        }
    }

    fn failed_with_usage(
        &self,
        request_id: u64,
        error_class: ErrorClass,
        error: String,
        usage: Usage,
        logs: String,
        retire: bool,
    ) -> CallOutcome {
        CallOutcome {
            result: GuestMessage::Result {
                request_id,
                ok: false,
                output: None,
                error: Some(error),
                error_class,
                truncated: true,
            },
            usage,
            retire,
            logs,
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

    fn diagnostic(&self) -> String {
        let status = self
            .process
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
            .map(|status| format!("worker exited with {status}"))
            .unwrap_or_else(|| "worker status unavailable".to_string());
        let logs = self.take_logs();
        if logs.text.is_empty() {
            status
        } else {
            format!("{status}; worker stderr: {}", logs.text.trim())
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

async fn write_line(writer: &mut ChildStdin, value: &Value) -> std::io::Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| std::io::Error::other(error.to_string()))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
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
