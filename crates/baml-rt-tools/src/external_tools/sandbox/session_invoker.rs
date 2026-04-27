//! Pool-backed [`SessionToolInvoker`] for `invocation_mode=session` tools.
//!
//! Implements `plans/sandbox_streaming.md` §5–§7 host side: each
//! [`session_open`](SessionToolInvoker::session_open) checks out a sandbox
//! from the [`SessionPool`], opens a persistent [`TsrpcChannel`] over
//! `provider.rpc_channel(handle)`, and registers a live session entry. All
//! subsequent `session_*` RPCs reuse that channel under a single-reader lock.
//! `session_finish` runs the optional `tool/session_reset` hook before
//! returning the sandbox to `Idle`; `session_abort` (or any reset failure)
//! tears the sandbox down.
//!
//! Per-tool reuse policy is configured via [`SandboxSessionInvokerConfig`]
//! and passed at construction time. Phase 4 will wire this from
//! [`super::super::ExternalToolMetadata::session_policy`] +
//! `reuse_after_session`.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, ClassifiedToolError, ContextId, ErrorDisposition, Result, ids::AgentId,
};
use baml_sandbox_protocol::{
    JsonRpcRequest, JsonRpcResponse, METHOD_SESSION_ABORT, METHOD_SESSION_FINISH,
    METHOD_SESSION_OPEN, METHOD_SESSION_READ, METHOD_SESSION_RESET, METHOD_SESSION_SEND,
    session::{
        SessionAbortParams, SessionAbortResult, SessionFinishParams, SessionFinishResult,
        SessionOpenParams, SessionOpenResult, SessionReadParams, SessionResetOutcome,
        SessionResetParams, SessionResetResult, SessionSendParams, SessionSendResult, StepEnvelope,
    },
};
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::{
    channel::TsrpcChannel,
    invoker::SandboxCacheKey,
    session_pool::{PooledSandbox, SessionPool},
};
use crate::{
    ToolName,
    external_tools::{
        invoker::map_jsonrpc_error,
        session_invoker::{
            SessionAbortRequest, SessionFinishRequest, SessionOpenRequest, SessionOpenResponse,
            SessionReadRequest, SessionSendRequest, SessionToolInvoker,
        },
    },
    tool_fsm::ToolStep,
};

/// Per-invoker configuration. One [`SandboxSessionInvoker`] is constructed
/// per `(agent_id, context_id)` scope; tool-level toggles (reuse, reset
/// timeout) come from metadata via this config.
#[derive(Debug, Clone)]
pub struct SandboxSessionInvokerConfig {
    /// Default RPC deadline used when a request omits its own timeout.
    pub default_rpc_timeout: Duration,
    /// Upper bound on `tool/session_reset`. Per §7.2 reset failure /
    /// timeout destroys the sandbox.
    pub reset_timeout: Duration,
    /// When `false`, the invoker destroys the sandbox after every
    /// `session_finish` regardless of `on_reset` outcome (default-safe per
    /// §7.2). When `true`, a successful reset returns the entry to `Idle`.
    pub reuse_after_session: bool,
}

impl Default for SandboxSessionInvokerConfig {
    fn default() -> Self {
        Self {
            default_rpc_timeout: Duration::from_secs(30),
            reset_timeout: Duration::from_secs(5),
            reuse_after_session: false,
        }
    }
}

/// Live session bookkeeping.
struct LiveSession {
    pooled: PooledSandbox,
    /// One-RPC-at-a-time mutex on the persistent adapter channel
    /// (single-reader invariant from §3.1).
    channel: Mutex<TsrpcChannel>,
    /// Session start time, useful for span attributes / max-duration eviction.
    #[allow(dead_code)]
    started_at: Instant,
    tool_name: ToolName,
}

/// Pool-backed concrete implementation of [`SessionToolInvoker`].
pub struct SandboxSessionInvoker {
    pool: Arc<SessionPool>,
    agent_id: AgentId,
    context_id: ContextId,
    config: SandboxSessionInvokerConfig,
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
}

impl SandboxSessionInvoker {
    pub fn new(
        pool: Arc<SessionPool>,
        agent_id: AgentId,
        context_id: ContextId,
        config: SandboxSessionInvokerConfig,
    ) -> Self {
        Self {
            pool,
            agent_id,
            context_id,
            config,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn key(&self, tool: &ToolName) -> SandboxCacheKey {
        SandboxCacheKey {
            agent_id: self.agent_id.clone(),
            context_id: self.context_id.clone(),
            tool_name: tool.clone(),
        }
    }

    async fn lookup_session(&self, session_id: &str) -> Result<Arc<LiveSession>> {
        let guard = self.sessions.lock().await;
        guard.get(session_id).cloned().ok_or_else(|| {
            BamlRtError::ToolClassified(ClassifiedToolError {
                code: baml_sandbox_protocol::session::error_code::UNKNOWN_SESSION.to_string(),
                disposition: ErrorDisposition::Fatal,
                message: format!("session '{session_id}' is not tracked by this invoker"),
                hint: None,
                retry_after_ms: None,
            })
        })
    }

    async fn remove_session(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.sessions.lock().await.remove(session_id)
    }
}

#[async_trait]
impl SessionToolInvoker for SandboxSessionInvoker {
    async fn session_open(&self, req: SessionOpenRequest) -> Result<SessionOpenResponse> {
        let key = self.key(&req.tool_name);
        let pooled = self.pool.checkout(&key).await?;
        let mut channel = self.pool.provider().rpc_channel(pooled.handle()).await?;

        let params = SessionOpenParams {
            invocation_id: req.invocation_id,
            tool_name: req.tool_name.to_string(),
            open_input: req.open_input,
            secrets: req.secrets,
            capabilities: req.capabilities,
        };

        let result: SessionOpenResult = match call_one_rpc(
            &req.tool_name,
            &mut channel,
            METHOD_SESSION_OPEN,
            params,
            req.timeout.max(self.config.default_rpc_timeout),
        )
        .await
        {
            Ok(r) => r,
            Err(err) => {
                // open failed before any tool output was committed — destroy
                // the sandbox and surface the classified error.
                pooled.release_destroy().await;
                return Err(err);
            }
        };

        let session = Arc::new(LiveSession {
            pooled,
            channel: Mutex::new(channel),
            started_at: Instant::now(),
            tool_name: req.tool_name.clone(),
        });

        let session_id = result.session_id.clone();
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), Arc::clone(&session));
        debug!(
            tool = %req.tool_name,
            session_id = %session_id,
            sandbox = %session.pooled.handle().name,
            "session opened"
        );

        Ok(SessionOpenResponse {
            session_id,
            initial_step: result.initial_step.map(step_envelope_to_tool_step),
        })
    }

    async fn session_send(&self, req: SessionSendRequest) -> Result<()> {
        let session = self.lookup_session(&req.session_id).await?;
        let mut channel = session.channel.lock().await;
        let params = SessionSendParams {
            session_id: req.session_id.clone(),
            input: req.input,
            resume_token: req.resume_token,
            secrets: req.secrets,
        };
        let _ack: SessionSendResult = call_one_rpc(
            &session.tool_name,
            &mut channel,
            METHOD_SESSION_SEND,
            params,
            req.timeout.max(self.config.default_rpc_timeout),
        )
        .await?;
        Ok(())
    }

    async fn session_read(&self, req: SessionReadRequest) -> Result<ToolStep> {
        let session = self.lookup_session(&req.session_id).await?;
        let mut channel = session.channel.lock().await;
        let params = SessionReadParams {
            session_id: req.session_id.clone(),
        };
        let envelope: StepEnvelope = call_one_rpc(
            &session.tool_name,
            &mut channel,
            METHOD_SESSION_READ,
            params,
            req.chunk_timeout.max(self.config.default_rpc_timeout),
        )
        .await?;
        Ok(step_envelope_to_tool_step(envelope))
    }

    async fn session_finish(&self, req: SessionFinishRequest) -> Result<()> {
        let Some(session) = self.remove_session(&req.session_id).await else {
            return Err(unknown_session_error(&req.session_id));
        };

        let outcome = {
            let mut channel = session.channel.lock().await;
            let params = SessionFinishParams {
                session_id: req.session_id.clone(),
            };
            call_one_rpc::<SessionFinishResult>(
                &session.tool_name,
                &mut channel,
                METHOD_SESSION_FINISH,
                params,
                req.timeout.max(self.config.default_rpc_timeout),
            )
            .await
        };

        let session = Arc::try_unwrap(session).map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "session '{}' has outstanding readers; cannot finish",
                req.session_id
            ))
        })?;
        let LiveSession {
            pooled,
            channel,
            tool_name,
            ..
        } = session;

        match outcome {
            Ok(_) => {
                let reuse_ok = if self.config.reuse_after_session {
                    let mut channel = channel.into_inner();
                    run_reset_hook(
                        &tool_name,
                        &mut channel,
                        &req.session_id,
                        self.config.reset_timeout,
                    )
                    .await
                } else {
                    false
                };

                if reuse_ok {
                    pooled.release_finish_idle().await;
                } else {
                    pooled.release_destroy().await;
                }
                Ok(())
            }
            Err(err) => {
                pooled.release_destroy().await;
                Err(err)
            }
        }
    }

    async fn session_abort(&self, req: SessionAbortRequest) -> Result<()> {
        let Some(session) = self.remove_session(&req.session_id).await else {
            return Err(unknown_session_error(&req.session_id));
        };

        let abort_outcome = {
            let mut channel = session.channel.lock().await;
            let params = SessionAbortParams {
                session_id: req.session_id.clone(),
                reason: req.reason,
            };
            call_one_rpc::<SessionAbortResult>(
                &session.tool_name,
                &mut channel,
                METHOD_SESSION_ABORT,
                params,
                req.timeout.max(self.config.default_rpc_timeout),
            )
            .await
        };

        let session = Arc::try_unwrap(session).map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "session '{}' has outstanding readers; cannot abort",
                req.session_id
            ))
        })?;
        // Force-abort always destroys the sandbox; the RPC is best-effort.
        session.pooled.release_destroy().await;

        if let Err(err) = abort_outcome {
            warn!(
                session_id = %req.session_id,
                error = %err,
                "session_abort RPC failed; sandbox destroyed via pool"
            );
        }
        Ok(())
    }
}

async fn run_reset_hook(
    tool: &ToolName,
    channel: &mut TsrpcChannel,
    session_id: &str,
    timeout: Duration,
) -> bool {
    let params = SessionResetParams {
        session_id: session_id.to_string(),
    };
    match call_one_rpc::<SessionResetResult>(tool, channel, METHOD_SESSION_RESET, params, timeout)
        .await
    {
        Ok(r) => matches!(r.outcome, SessionResetOutcome::Ok),
        Err(err) => {
            warn!(
                tool = %tool,
                session_id = %session_id,
                error = %err,
                "session_reset failed; sandbox will be destroyed"
            );
            false
        }
    }
}

fn unknown_session_error(session_id: &str) -> BamlRtError {
    BamlRtError::ToolClassified(ClassifiedToolError {
        code: baml_sandbox_protocol::session::error_code::UNKNOWN_SESSION.to_string(),
        disposition: ErrorDisposition::Fatal,
        message: format!("session '{session_id}' is not tracked by this invoker"),
        hint: None,
        retry_after_ms: None,
    })
}

async fn call_one_rpc<R: DeserializeOwned>(
    tool: &ToolName,
    channel: &mut TsrpcChannel,
    method: &str,
    params: impl serde::Serialize,
    timeout: Duration,
) -> Result<R> {
    let id = next_rpc_id();
    let params_json =
        serde_json::to_value(&params).map_err(|err| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to encode {method} params"),
            source: Box::new(err),
        })?;
    let request = JsonRpcRequest::new(method, id, params_json);
    let request_value =
        serde_json::to_value(&request).map_err(|err| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to encode JSON-RPC request for {method}"),
            source: Box::new(err),
        })?;

    let exchange = async {
        channel.send(&request_value).await.map_err(|err| {
            BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to send TSRPC frame for {method}"),
                source: Box::new(err),
            }
        })?;
        channel
            .recv()
            .await
            .map_err(|err| BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to recv TSRPC frame for {method}"),
                source: Box::new(err),
            })
    };

    let response_value = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "session RPC '{method}' for '{tool}' timed out after {timeout:?}"
            ))
        })??;

    let response: JsonRpcResponse = serde_json::from_value(response_value).map_err(|err| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to decode JSON-RPC response for {method}"),
            source: Box::new(err),
        }
    })?;
    if let Some(rpc_err) = response.error {
        return Err(map_jsonrpc_error(tool, &rpc_err));
    }
    let raw = response.result.ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "JSON-RPC response for {method} missing result payload"
        ))
    })?;
    serde_json::from_value(raw).map_err(|err| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to decode {method} result payload"),
        source: Box::new(err),
    })
}

fn next_rpc_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn step_envelope_to_tool_step(envelope: StepEnvelope) -> ToolStep {
    use crate::{
        tool_error_classify::ClassifiedToolError as CoreClassified,
        tool_fsm::{ToolFailure, ToolFailureKind},
    };

    match envelope {
        StepEnvelope::Streaming { output } => ToolStep::Streaming { output },
        // Phase 1 of the host runtime does not yet plumb adapter resume
        // tokens through `ToolStep` — surface as Suspended without it for
        // now; Phase 4 wires the resume_token into the ExternalSession
        // bookkeeping.
        StepEnvelope::Suspended {
            output,
            resume_token: _,
        } => ToolStep::Suspended { output },
        StepEnvelope::Done { output } => ToolStep::Done { output },
        StepEnvelope::Error { error } => {
            let disposition = match error.disposition {
                baml_sandbox_protocol::session::SessionDisposition::HostRetriable => {
                    ErrorDisposition::HostRetriable
                }
                baml_sandbox_protocol::session::SessionDisposition::InformAndContinue => {
                    ErrorDisposition::InformAndContinue
                }
                baml_sandbox_protocol::session::SessionDisposition::LlmCorrectable => {
                    ErrorDisposition::LlmCorrectable
                }
                baml_sandbox_protocol::session::SessionDisposition::Fatal => {
                    ErrorDisposition::Fatal
                }
            };
            let classified = CoreClassified {
                code: error.code,
                disposition,
                message: error.message.clone(),
                hint: error.hint,
                retry_after_ms: error.retry_after_ms,
            };
            let kind = match disposition {
                ErrorDisposition::HostRetriable => ToolFailureKind::ExecutionFailed,
                ErrorDisposition::LlmCorrectable => ToolFailureKind::InvalidInput,
                ErrorDisposition::InformAndContinue => ToolFailureKind::ExecutionFailed,
                ErrorDisposition::Fatal => ToolFailureKind::ExecutionFailed,
            };
            ToolStep::Error {
                error: ToolFailure {
                    kind,
                    message: error.message,
                    retryability: match disposition {
                        ErrorDisposition::HostRetriable => baml_rt_core::Retryability::Retryable,
                        _ => baml_rt_core::Retryability::Permanent,
                    },
                    classified,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use baml_rt_core::{
        ContextId,
        ids::{AgentId, UuidId},
    };
    use baml_sandbox_protocol::{
        JsonRpcResponse,
        session::{SessionDisposition, StepError, error_code},
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        SandboxSessionInvoker, SandboxSessionInvokerConfig, SessionOpenRequest, SessionReadRequest,
        SessionSendRequest, SessionToolInvoker,
    };
    use crate::{
        ToolName,
        external_tools::sandbox::{
            SandboxProvider, SandboxSpec, SandboxSpecBuilder,
            mock::{MockSandboxProvider, ScriptedAdapter},
            session_pool::{SessionPool, SessionPoolConfig},
        },
        tool_fsm::ToolStep,
    };

    #[tokio::test]
    async fn session_streaming_happy_path_reads_chunks_then_done() {
        let adapter = streaming_meteo_adapter();
        let provider_concrete = MockSandboxProvider::new(adapter);
        let provider: Arc<dyn SandboxProvider> = Arc::new(provider_concrete);

        let tool_name = ToolName::parse("support/stream_weather").unwrap();
        let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        let context_id = ContextId::new(10, 20);

        let build_spec: SandboxSpecBuilder = Arc::new(|key| {
            Ok(SandboxSpec::for_test(
                format!("test-{}", key.tool_name),
                "scratch:latest",
            ))
        });

        let pool = Arc::new(SessionPool::new(
            "runner-test",
            provider,
            build_spec,
            SessionPoolConfig {
                default_pool_max: 1,
                pool_checkout_timeout: Duration::from_millis(200),
            },
        ));

        let invoker = SandboxSessionInvoker::new(
            pool.clone(),
            agent_id,
            context_id,
            SandboxSessionInvokerConfig::default(),
        );

        let open = invoker
            .session_open(SessionOpenRequest {
                tool_name: tool_name.clone(),
                invocation_id: "inv-stream-1".to_string(),
                open_input: Value::Null,
                secrets: serde_json::Map::new(),
                capabilities: Value::Null,
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("session_open should succeed");

        invoker
            .session_send(SessionSendRequest {
                session_id: open.session_id.clone(),
                input: json!({"location_query": "Quebec, Canada"}),
                resume_token: None,
                secrets: serde_json::Map::new(),
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("session_send should succeed");

        let step_1 = invoker
            .session_read(SessionReadRequest {
                session_id: open.session_id.clone(),
                chunk_timeout: Duration::from_secs(2),
            })
            .await
            .expect("first read");
        assert!(matches!(step_1, ToolStep::Streaming { .. }));

        let step_2 = invoker
            .session_read(SessionReadRequest {
                session_id: open.session_id.clone(),
                chunk_timeout: Duration::from_secs(2),
            })
            .await
            .expect("second read");

        let done_output = match step_2 {
            ToolStep::Done { output } => output.expect("done output"),
            other => panic!("expected Done, got {other:?}"),
        };
        assert_eq!(
            done_output
                .pointer("/location/query")
                .and_then(Value::as_str),
            Some("Quebec, Canada")
        );
        assert_eq!(
            done_output
                .pointer("/current/temperature_2m")
                .and_then(Value::as_f64),
            Some(-6.2)
        );

        invoker
            .session_finish(super::SessionFinishRequest {
                session_id: open.session_id,
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("session_finish should succeed");

        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn session_error_step_maps_resume_token_mismatch_classification() {
        let adapter = resume_token_error_adapter();
        let provider: Arc<dyn SandboxProvider> = Arc::new(MockSandboxProvider::new(adapter));

        let tool_name = ToolName::parse("support/stream_weather_error").unwrap();
        let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        let context_id = ContextId::new(10, 21);

        let build_spec: SandboxSpecBuilder = Arc::new(|key| {
            Ok(SandboxSpec::for_test(
                format!("test-{}", key.tool_name),
                "scratch:latest",
            ))
        });

        let pool = Arc::new(SessionPool::new(
            "runner-test",
            provider,
            build_spec,
            SessionPoolConfig {
                default_pool_max: 1,
                pool_checkout_timeout: Duration::from_millis(200),
            },
        ));

        let invoker = SandboxSessionInvoker::new(
            pool,
            agent_id,
            context_id,
            SandboxSessionInvokerConfig::default(),
        );

        let open = invoker
            .session_open(SessionOpenRequest {
                tool_name: tool_name.clone(),
                invocation_id: "inv-stream-err-1".to_string(),
                open_input: Value::Null,
                secrets: serde_json::Map::new(),
                capabilities: Value::Null,
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("session_open should succeed");

        let step = invoker
            .session_read(SessionReadRequest {
                session_id: open.session_id,
                chunk_timeout: Duration::from_secs(2),
            })
            .await
            .expect("read should succeed with error step envelope");

        match step {
            ToolStep::Error { error } => {
                assert_eq!(error.classified.code, error_code::RESUME_TOKEN_MISMATCH);
                assert_eq!(
                    error.classified.disposition,
                    baml_rt_core::ErrorDisposition::LlmCorrectable
                );
            }
            other => panic!("expected ToolStep::Error, got {other:?}"),
        }
    }

    fn streaming_meteo_adapter() -> ScriptedAdapter {
        Arc::new(|stream| {
            tokio::spawn(async move {
                let (mut r, mut w) = tokio::io::split(stream);
                let mut read_count = 0usize;
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
                    let req: Value = match serde_json::from_slice(&body) {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    let id = req.get("id").and_then(Value::as_u64).unwrap_or(1);
                    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                    let response = match method {
                        "tool/session_open" => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "session_id": "sess-weather-1" }
                        }),
                        "tool/session_send" => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {}
                        }),
                        "tool/session_read" => {
                            read_count += 1;
                            if read_count == 1 {
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "step": "streaming",
                                        "output": { "chunk": "Fetching weather for Quebec, Canada..." }
                                    }
                                })
                            } else {
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "step": "done",
                                        "output": {
                                            "source": "mock-meteo",
                                            "location": { "query": "Quebec, Canada", "country": "Canada" },
                                            "current": { "temperature_2m": -6.2, "wind_speed_10m": 12.4 }
                                        }
                                    }
                                })
                            }
                        }
                        "tool/session_finish" => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {}
                        }),
                        _ => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": "method not found" }
                        }),
                    };

                    let out = serde_json::to_vec(&response).unwrap();
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
        })
    }

    fn resume_token_error_adapter() -> ScriptedAdapter {
        Arc::new(|stream| {
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
                    let req: Value = match serde_json::from_slice(&body) {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    let id = req.get("id").and_then(Value::as_u64).unwrap_or(1);
                    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                    let response: JsonRpcResponse = match method {
                        "tool/session_open" => JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({ "session_id": "sess-weather-err-1" })),
                            error: None,
                        },
                        "tool/session_read" => JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({
                                "step": "error",
                                "error": StepError {
                                    code: error_code::RESUME_TOKEN_MISMATCH.to_string(),
                                    message: "resume token mismatch".to_string(),
                                    disposition: SessionDisposition::LlmCorrectable,
                                    hint: Some("send the adapter-provided resume_token".to_string()),
                                    retry_after_ms: None,
                                }
                            })),
                            error: None,
                        },
                        _ => JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({})),
                            error: None,
                        },
                    };

                    let out = serde_json::to_vec(&response).unwrap();
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
        })
    }
}
