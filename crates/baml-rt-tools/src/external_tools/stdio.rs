//! Stateless subprocess transport: spawn the tool binary per-invocation, speak
//! JSON-RPC over stdin/stdout.
//!
//! Wire contract enforced here:
//! - stdout: exactly one JSON-RPC frame per spawn (followed by EOF).
//! - stderr: captured and forwarded to `tracing` at debug level.
//! - non-JSON on stdout ⇒ protocol error (`BamlRtError::InvalidArgument`).
//! - no shared state across calls; each invocation spawns a fresh process.

use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};
use tracing::{debug, warn};

use super::{
    invoker::{ExternalInvoker, InvokeRequest, InvokeResponse, ToolDescribe, map_jsonrpc_error},
    protocol::{
        JsonRpcRequest, JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, ToolDescribeResult,
        ToolInvokeParams, ToolInvokeResult,
    },
};
use crate::ToolName;

/// V1 default invoker: spawn subprocess per call, talk JSON-RPC over stdio.
pub struct StdioSubprocessInvoker {
    executable: PathBuf,
    /// Environment variables passed to the child process. Secrets are NOT
    /// inherited from the runner's ambient env — only values placed here
    /// (typically by the resolver at load time) are visible to the tool.
    env: HashMap<String, String>,
    /// Optional working directory for the child.
    working_dir: Option<PathBuf>,
    /// Monotonic id generator for JSON-RPC request ids.
    next_id: AtomicU64,
}

impl StdioSubprocessInvoker {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            env: HashMap::new(),
            working_dir: None,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Spawn the tool, send one JSON-RPC frame, read one frame back, kill on timeout.
    async fn call_once(
        &self,
        request: &JsonRpcRequest,
        call_timeout: Duration,
    ) -> Result<JsonRpcResponse> {
        let mut cmd = Command::new(&self.executable);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Kill the child if we drop the Child handle (timeout or error path).
            .kill_on_drop(true)
            // Clear inherited env; tool sees only what we explicitly pass.
            .env_clear()
            .envs(&self.env);
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: format!(
                    "failed to spawn external tool: {}",
                    self.executable.display()
                ),
                source: Box::new(e),
            })?;

        let frame =
            serde_json::to_vec(request).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to serialize JSON-RPC request".into(),
                source: Box::new(e),
            })?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BamlRtError::InvalidArgument("child has no stdin".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| BamlRtError::InvalidArgument("child has no stdout".into()))?;
        let stderr_opt = child.stderr.take();

        let interaction = async move {
            // Write request frame + newline + EOF.
            stdin.write_all(&frame).await?;
            stdin.write_all(b"\n").await?;
            stdin.shutdown().await?;
            drop(stdin);

            // Read stdout fully (tool must emit exactly one frame then exit).
            let mut stdout_buf = Vec::new();
            stdout.read_to_end(&mut stdout_buf).await?;

            // Drain stderr → tracing.
            if let Some(mut stderr) = stderr_opt {
                let mut stderr_buf = Vec::new();
                let _ = stderr.read_to_end(&mut stderr_buf).await;
                if !stderr_buf.is_empty() {
                    let log = String::from_utf8_lossy(&stderr_buf);
                    debug!(tool_stderr = %log, "external tool stderr");
                }
            }

            let status = child.wait().await?;
            Ok::<(std::process::ExitStatus, Vec<u8>), std::io::Error>((status, stdout_buf))
        };

        let (status, stdout_buf) = match timeout(call_timeout, interaction).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(io_err)) => {
                return Err(BamlRtError::InvalidArgumentWithSource {
                    message: "I/O error talking to external tool".into(),
                    source: Box::new(io_err),
                });
            }
            Err(_elapsed) => {
                // Timed out — interaction future is dropped, kill_on_drop fires.
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool timed out after {:?}",
                    call_timeout
                )));
            }
        };

        if !status.success() {
            warn!(exit = ?status, "external tool exited with non-zero status");
        }

        // Enforce stdout-JSON-only contract.
        let stdout_str = std::str::from_utf8(&stdout_buf).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "external tool stdout is not valid UTF-8 (protocol error)".into(),
                source: Box::new(e),
            }
        })?;
        let trimmed = stdout_str.trim();
        if trimmed.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "external tool produced no JSON-RPC frame on stdout (protocol error)".into(),
            ));
        }

        serde_json::from_str::<JsonRpcResponse>(trimmed).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: format!("malformed JSON-RPC frame from external tool: {trimmed}"),
                source: Box::new(e),
            }
        })
    }
}

#[async_trait]
impl ExternalInvoker for StdioSubprocessInvoker {
    async fn describe(&self, tool: &ToolName, call_timeout: Duration) -> Result<ToolDescribe> {
        let params = serde_json::json!({ "tool_name": tool.to_string() });
        let request = JsonRpcRequest::new(METHOD_DESCRIBE, self.next_request_id(), params);
        let response = self.call_once(&request, call_timeout).await?;

        if let Some(err) = response.error {
            return Err(map_jsonrpc_error(tool, &err));
        }
        let result_value = response.result.ok_or_else(|| {
            BamlRtError::InvalidArgument("tool/describe: missing result field".into())
        })?;
        let describe: ToolDescribeResult = serde_json::from_value(result_value).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "tool/describe: result did not match schema".into(),
                source: Box::new(e),
            }
        })?;
        Ok(describe.into())
    }

    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse> {
        let params = ToolInvokeParams {
            invocation_id: req.invocation_id,
            tool_name: req.tool_name.to_string(),
            input: req.input,
            secrets: req.secrets,
            capabilities: req.capabilities,
        };
        let params_value =
            serde_json::to_value(&params).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to serialize tool/invoke params".into(),
                source: Box::new(e),
            })?;
        let request = JsonRpcRequest::new(METHOD_INVOKE, self.next_request_id(), params_value);
        let response = self.call_once(&request, req.timeout).await?;

        if let Some(err) = response.error {
            return Err(map_jsonrpc_error(&req.tool_name, &err));
        }
        let result_value = response.result.ok_or_else(|| {
            BamlRtError::InvalidArgument("tool/invoke: missing result field".into())
        })?;
        let invoke_result: ToolInvokeResult =
            serde_json::from_value(result_value).map_err(|e| {
                BamlRtError::InvalidArgumentWithSource {
                    message: "tool/invoke: result did not match schema".into(),
                    source: Box::new(e),
                }
            })?;
        Ok(InvokeResponse {
            output: invoke_result.output,
            done: invoke_result.done,
        })
    }
}
