//! `ToolHandler` and `ToolSession` implementations for MCP-imported tools.
//!
//! One `McpToolHandler` per imported tool. Multiple handlers from the same
//! server share a single [`McpConnection`], so the runtime spawns one MCP
//! child per (server-id, server-config) regardless of how many tools point
//! at it.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, ClassifiedToolError, Result, semantics::ErrorDisposition};
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
            debug_assert!(
                false,
                "MCP runtime received non-object tool arguments: {err}"
            );
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
        ConnectionError::CallTool(ref service_err) => classify_service_error(&err, service_err),
        ConnectionError::InitializeFailed(_) => classified_transport(
            "mcp_initialize_failed",
            ErrorDisposition::HostRetriable,
            err.to_string(),
            Some("MCP initialize failed before tool call; host may retry after transport recovery"),
        ),
        ConnectionError::InitializeTimeout(_) => classified_transport(
            "mcp_initialize_timeout",
            ErrorDisposition::HostRetriable,
            err.to_string(),
            Some("MCP initialize exceeded configured timeout"),
        ),
        ConnectionError::CallTimeout(_) => classified_transport(
            "mcp_call_timeout",
            ErrorDisposition::HostRetriable,
            err.to_string(),
            Some("MCP tools/call exceeded configured timeout and rmcp sent cancellation"),
        ),
        ConnectionError::IdentityMismatch { .. }
        | ConnectionError::ToolsDigestMismatch { .. }
        | ConnectionError::StartupToolsListFailed { .. }
        | ConnectionError::MissingPeerInfo { .. }
        | ConnectionError::IdentitySerializeFailed { .. }
        | ConnectionError::Stale { .. } => classified_transport(
            "mcp_contract_violation",
            ErrorDisposition::Fatal,
            err.to_string(),
            Some(
                "MCP approved snapshot no longer matches live server; operator must re-import and approve",
            ),
        ),
        ConnectionError::SessionExpired { .. } => classified_transport(
            "mcp_session_expired",
            ErrorDisposition::InformAndContinue,
            err.to_string(),
            Some(
                "MCP HTTP session expired; next resolve rebuilds connection, but this call is not replayed",
            ),
        ),
        ConnectionError::Spawn { .. } => classified_transport(
            "mcp_spawn_failed",
            ErrorDisposition::HostRetriable,
            err.to_string(),
            Some("MCP stdio server failed to spawn"),
        ),
        ConnectionError::Transport(_) => classified_transport(
            "mcp_transport_setup_failed",
            ErrorDisposition::Fatal,
            err.to_string(),
            Some("MCP transport setup or policy validation failed"),
        ),
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
        ServiceError::TransportSend(_) => classified_transport(
            "mcp_transport_send_failed",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP transport failed while sending request"),
        ),
        ServiceError::TransportClosed => classified_transport(
            "mcp_transport_closed",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP transport closed before response"),
        ),
        ServiceError::UnexpectedResponse => classified_transport(
            "mcp_protocol_unexpected_response",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP server returned a response type that does not match tools/call"),
        ),
        ServiceError::Cancelled { .. } => classified_transport(
            "mcp_call_cancelled",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP request was cancelled"),
        ),
        ServiceError::Timeout { .. } => classified_transport(
            "mcp_call_timeout",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP request timed out"),
        ),
        _ => classified_transport(
            "mcp_service_error",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP service returned an unclassified error"),
        ),
    }
}

fn classified_transport(
    code: &'static str,
    disposition: ErrorDisposition,
    message: String,
    hint: Option<&'static str>,
) -> ToolSessionError {
    ToolSessionError::Transport(BamlRtError::ToolClassified(ClassifiedToolError {
        code: code.to_string(),
        disposition,
        message,
        hint: hint.map(str::to_string),
        retry_after_ms: None,
    }))
}

/// Convert rmcp's `CallToolResult` into the platform's stable JSON envelope.
/// The shape mirrors the `ContentEnvelope` output mode locked in PR 1.
#[cfg(test)]
mod tests {
    use super::*;

    fn classified(err: ToolSessionError) -> ClassifiedToolError {
        match err {
            ToolSessionError::Transport(BamlRtError::ToolClassified(classified)) => classified,
            other => panic!("expected classified transport error, got {other:?}"),
        }
    }

    #[test]
    fn session_expired_maps_to_typed_classification() {
        let err = connection_error_to_session(ConnectionError::SessionExpired {
            server_id: "remote".into(),
        });
        let classified = classified(err);
        assert_eq!(classified.code, "mcp_session_expired");
        assert_eq!(classified.disposition, ErrorDisposition::InformAndContinue);
    }

    #[test]
    fn digest_mismatch_maps_to_contract_violation_not_invalid_argument() {
        let err = connection_error_to_session(ConnectionError::ToolsDigestMismatch {
            server_id: "remote".into(),
            expected: "sha256:old".into(),
            observed: "sha256:new".into(),
        });
        let classified = classified(err);
        assert_eq!(classified.code, "mcp_contract_violation");
        assert_eq!(classified.disposition, ErrorDisposition::Fatal);
    }
}

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
