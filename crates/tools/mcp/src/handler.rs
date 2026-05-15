//! `ToolHandler` and `ToolSession` implementations for MCP-imported tools.
//!
//! One `McpToolHandler` per imported tool. Multiple handlers from the same
//! server share a single [`McpConnection`], so the runtime spawns one MCP
//! child per (server-id, server-config) regardless of how many tools point
//! at it.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolFailure, ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};
use rmcp::{
    model::{CallToolResult, ErrorCode},
    service::ServiceError,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::runtime::{ConnectionError, McpConnection};

pub struct McpToolHandler {
    metadata: ToolFunctionMetadata,
    connection: Arc<McpConnection>,
    mcp_tool_name: String,
    schema_digest: String,
}

impl McpToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        connection: Arc<McpConnection>,
        mcp_tool_name: String,
        schema_digest: String,
    ) -> Self {
        Self {
            metadata,
            connection,
            mcp_tool_name,
            schema_digest,
        }
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(
        &self,
        _ctx: ToolSessionContext,
        _open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        Ok(Box::new(McpToolSession {
            connection: self.connection.clone(),
            mcp_tool_name: self.mcp_tool_name.clone(),
            schema_digest: self.schema_digest.clone(),
            state: Mutex::new(SessionState::Idle),
        }))
    }
}

#[derive(Debug, Default)]
enum SessionState {
    #[default]
    Idle,
    /// `send` has produced a result that `read` is expected to consume.
    Ready(Value),
    Aborted,
    Closed,
}

struct McpToolSession {
    connection: Arc<McpConnection>,
    mcp_tool_name: String,
    schema_digest: String,
    state: Mutex<SessionState>,
}

#[async_trait]
impl ToolSession for McpToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        {
            let state = self.state.lock().await;
            match &*state {
                SessionState::Aborted => {
                    return Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                        "MCP session already aborted",
                    )));
                }
                SessionState::Closed => {
                    return Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                        "MCP session already closed",
                    )));
                }
                SessionState::Ready(_) => {
                    return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                        "MCP session already has an unread result; call read before sending again",
                    )));
                }
                SessionState::Idle => {}
            }
            // Hold the lock only while validating state, drop before awaiting MCP.
            drop(state);
        }
        let call_span = tracing::info_span!(
            "mcp.call_tool",
            mcp_server_id = %self.connection.server_id(),
            mcp_tool_name = %self.mcp_tool_name,
            mcp_schema_digest = %self.schema_digest,
            mcp_protocol_version = %self.connection.protocol_version(),
            mcp_server_config_digest = %self.connection.server_config_digest(),
        );
        let result = {
            let _guard = call_span.enter();
            self.connection
                .call_tool(&self.mcp_tool_name, input)
                .await
                .map_err(connection_error_to_session)?
        };
        let envelope = result_to_envelope(result);
        let mut state = self.state.lock().await;
        *state = SessionState::Ready(envelope);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let mut state = self.state.lock().await;
        match std::mem::replace(&mut *state, SessionState::Closed) {
            SessionState::Ready(output) => Ok(ToolStep::Done {
                output: Some(output),
            }),
            SessionState::Aborted => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session aborted",
            ))),
            SessionState::Closed => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session already closed",
            ))),
            SessionState::Idle => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session has no pending result to read",
            ))),
        }
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        let mut state = self.state.lock().await;
        // Do not erase an `Aborted` marker — later observers (and the
        // session FSM itself) rely on `Aborted` being terminal.
        if !matches!(*state, SessionState::Aborted) {
            *state = SessionState::Closed;
        }
        Ok(())
    }

    /// Marks the session as aborted **locally**. The in-flight `tools/call`
    /// on the shared MCP connection keeps running until the server completes
    /// or the per-call timeout fires; `notifications/cancelled` per-request
    /// is not yet plumbed through rmcp's public `call_tool` surface.
    /// Callers therefore should not assume cancellation propagates to the
    /// MCP peer — the local session refuses further reads, but the server
    /// may still produce side effects from the request that was already
    /// dispatched.
    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        let mut state = self.state.lock().await;
        *state = SessionState::Aborted;
        Ok(())
    }
}

fn connection_error_to_session(err: ConnectionError) -> ToolSessionError {
    match err {
        ConnectionError::InvalidArguments(_) => {
            // BAML codegen guarantees a JSON object for tool arguments. Reaching
            // this arm means the runtime received a malformed payload — surface
            // as a transport/infra error so the LLM does not get prompted to
            // "retry with different inputs," which would mask the codegen bug.
            debug_assert!(false, "MCP runtime received non-object tool arguments: {err}");
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
        ConnectionError::CallTool(ref service_err) => classify_service_error(&err, service_err),
        ConnectionError::InitializeFailed(_) | ConnectionError::CallTimeout(_) => {
            ToolSessionError::Tool(ToolFailure::execution_failed(err.to_string()))
        }
        ConnectionError::InitializeTimeout(_) => {
            ToolSessionError::Tool(ToolFailure::execution_failed(err.to_string()))
        }
        ConnectionError::IdentityMismatch { .. }
        | ConnectionError::MissingPeerInfo { .. }
        | ConnectionError::IdentitySerializeFailed { .. } => {
            // Fail-closed: the live server's advertised identity does not
            // match the approved snapshot, was missing, or could not be
            // serialized for the digest. Treat as transport failure so the
            // runtime does not surface this to the LLM as a recoverable
            // tool error.
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
        ConnectionError::Stale { .. } => {
            // Fail-closed transport error: the tool registry is no longer
            // trusted for this connection; surface to the runtime so the
            // session is treated as an infra problem, not LLM-correctable.
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
        ConnectionError::Spawn { .. } | ConnectionError::Transport(_) => {
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
    }
}

/// Map rmcp's `ServiceError` to our `ToolSessionError` with disposition
/// based on JSON-RPC error code: codes that indicate the caller can adjust
/// inputs become `invalid_input` (LLM-correctable); everything else is a
/// hard execution failure.
fn classify_service_error(top: &ConnectionError, err: &ServiceError) -> ToolSessionError {
    match err {
        ServiceError::McpError(data) => {
            let code = data.code;
            if code == ErrorCode::INVALID_PARAMS
                || code == ErrorCode::METHOD_NOT_FOUND
                || code == ErrorCode::INVALID_REQUEST
            {
                ToolSessionError::Tool(ToolFailure::invalid_input(top.to_string()))
            } else {
                ToolSessionError::Tool(ToolFailure::execution_failed(top.to_string()))
            }
        }
        // Transport / cancellation / shutdown — treat as infra failure.
        _ => ToolSessionError::Transport(BamlRtError::InvalidArgument(top.to_string())),
    }
}

/// Convert rmcp's `CallToolResult` into the platform's stable JSON envelope.
/// The shape mirrors the `ContentEnvelope` output mode locked in PR 1.
fn result_to_envelope(result: CallToolResult) -> Value {
    let mut content = Vec::with_capacity(result.content.len());
    for block in &result.content {
        match serde_json::to_value(block.raw.clone()) {
            Ok(value) => content.push(value),
            Err(err) => {
                tracing::warn!(error = %err, "failed to serialize MCP content block");
            }
        }
    }
    let structured = result
        .structured_content
        .map(|sc| serde_json::to_value(sc).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    json!({
        "content": content,
        "structured": structured,
        "is_error": result.is_error.unwrap_or(false),
        "metadata": Value::Null,
    })
}
