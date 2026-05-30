// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `ToolHandler` and `ToolSession` implementations for MCP-imported tools.
//!
//! One `McpToolHandler` per imported tool. Multiple handlers from the same
//! server share a single [`McpConnection`], so the runtime spawns one MCP
//! child per (server-id, server-config) regardless of how many tools point
//! at it.

use std::{error::Error as StdError, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, ClassifiedToolError, Result, semantics::ErrorDisposition};
use baml_rt_tools::{
    ToolFailure, ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};
use rmcp::{
    model::{CallToolResult, ErrorCode},
    service::ServiceError,
    transport::streamable_http_client::StreamableHttpError,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    http::transport::HttpTransportBuildError,
    runtime::{ConnectionError, McpCancelSlot, McpConnection},
};

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
            cancel_slot: Default::default(),
            state: Mutex::new(SessionState::Idle),
        }))
    }
}

#[derive(Debug, Default)]
enum SessionState {
    #[default]
    Idle,
    /// `send` has reserved the session while the remote MCP call is in flight.
    /// This prevents a concurrent/buggy caller from starting a second remote
    /// call and causing duplicate side effects before either result is ready.
    InFlight,
    /// `send` has produced a result that `read` is expected to consume.
    Ready(Value),
    Aborted,
    Closed,
}

struct McpToolSession {
    connection: Arc<McpConnection>,
    mcp_tool_name: String,
    schema_digest: String,
    cancel_slot: McpCancelSlot,
    state: Mutex<SessionState>,
}

#[async_trait]
impl ToolSession for McpToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        {
            let mut state = self.state.lock().await;
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
                SessionState::InFlight => {
                    return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                        "MCP session already has a call in flight; wait for it before sending again",
                    )));
                }
                SessionState::Ready(_) => {
                    return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                        "MCP session already has an unread result; call read before sending again",
                    )));
                }
                SessionState::Idle => {
                    *state = SessionState::InFlight;
                }
            }
            // Hold the lock only while reserving the session, drop before awaiting MCP.
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
                .call_tool_with_cancel_slot(
                    &self.mcp_tool_name,
                    input,
                    Some(self.cancel_slot.clone()),
                )
                .await
        };
        let result = match result {
            Ok(result) => result,
            Err(err) => {
                let mut state = self.state.lock().await;
                if matches!(*state, SessionState::InFlight) {
                    *state = SessionState::Idle;
                }
                return Err(connection_error_to_session(err));
            }
        };
        let envelope = result_to_envelope(result);
        let mut state = self.state.lock().await;
        match &*state {
            SessionState::InFlight => {
                *state = SessionState::Ready(envelope);
                Ok(())
            }
            SessionState::Aborted => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session aborted after remote call completed; result discarded",
            ))),
            SessionState::Closed => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session closed after remote call completed; result discarded",
            ))),
            SessionState::Ready(_) | SessionState::Idle => {
                Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                    "MCP session state changed unexpectedly after remote call completed; result discarded",
                )))
            }
        }
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
            SessionState::InFlight => {
                *state = SessionState::InFlight;
                Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                    "MCP session has a call in flight; result is not ready to read",
                )))
            }
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

    /// Marks the session as aborted locally, cancels the local in-flight
    /// wait, and sends a bounded best-effort `notifications/cancelled` to
    /// the MCP peer. The peer may still complete work if its transport or
    /// server implementation does not observe cancellation before side
    /// effects occur.
    async fn abort(&mut self, reason: Option<String>) -> std::result::Result<(), ToolSessionError> {
        let cancel_handle = self.cancel_slot.lock().await.take();
        if let Some(handle) = cancel_handle {
            let cancel_result = handle.cancel(reason).await;
            if let Err(err) = cancel_result {
                tracing::warn!(
                    target: "mcp.runtime",
                    mcp_server_id = %self.connection.server_id(),
                    error = %err,
                    event = "mcp.call_cancel_notify_failed",
                    "failed to send MCP cancelled notification during abort",
                );
            }
        }
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
        ConnectionError::InitializeFailed(_) => retriable_transport(
            "mcp_initialize_failed",
            err.to_string(),
            "MCP initialize failed before tool call; host may retry after transport recovery",
        ),
        ConnectionError::InitializeTimeout(_) => retriable_transport(
            "mcp_initialize_timeout",
            err.to_string(),
            "MCP initialize exceeded configured timeout",
        ),
        ConnectionError::CallCancelled { .. } => inform_transport(
            "mcp_call_cancelled",
            err.to_string(),
            "MCP tools/call was cancelled locally and a best-effort cancellation notification was sent",
        ),
        ConnectionError::CallTimeout(_) => retriable_transport(
            "mcp_call_timeout",
            err.to_string(),
            "MCP tools/call exceeded configured timeout and rmcp sent cancellation",
        ),
        ConnectionError::IdentityMismatch { .. }
        | ConnectionError::ToolsDigestMismatch { .. }
        | ConnectionError::StartupToolsListFailed { .. }
        | ConnectionError::MissingPeerInfo { .. }
        | ConnectionError::IdentitySerializeFailed { .. }
        | ConnectionError::SnapshotStale { .. } => fatal_transport(
            "mcp_contract_violation",
            err.to_string(),
            "MCP approved snapshot no longer matches live server; operator must re-import and approve",
        ),
        ConnectionError::SessionExpired { .. } => inform_transport(
            "mcp_session_expired",
            err.to_string(),
            "MCP HTTP session expired; next resolve rebuilds connection, but this call is not replayed",
        ),
        ConnectionError::Spawn { .. } => retriable_transport(
            "mcp_spawn_failed",
            err.to_string(),
            "MCP stdio server failed to spawn",
        ),
        ConnectionError::Transport(ref source) => classify_transport_build_error(&err, source),
    }
}

fn classify_transport_build_error(
    top: &ConnectionError,
    err: &HttpTransportBuildError,
) -> ToolSessionError {
    match err {
        HttpTransportBuildError::Policy(_) => fatal_transport(
            "mcp_transport_policy_rejected",
            top.to_string(),
            "MCP HTTP transport network policy rejected the configured target",
        ),
        HttpTransportBuildError::Header(_) => fatal_transport(
            "mcp_transport_header_rejected",
            top.to_string(),
            "MCP HTTP transport static header configuration was rejected",
        ),
        HttpTransportBuildError::InvalidAuthHeader { .. } => fatal_transport(
            "mcp_transport_auth_header_invalid",
            top.to_string(),
            "MCP HTTP transport auth secret produced an invalid header",
        ),
        HttpTransportBuildError::ReservedAuthHeader { .. } => fatal_transport(
            "mcp_transport_auth_header_reserved",
            top.to_string(),
            "MCP HTTP transport auth secret targeted a reserved header name",
        ),
        HttpTransportBuildError::StdioSecretOnHttp { .. } => fatal_transport(
            "mcp_transport_stdio_secret_on_http",
            top.to_string(),
            "MCP HTTP transport rejected stdio-style env secret; declare credentials under transport `auth` block",
        ),
        HttpTransportBuildError::InvalidExtraCaCert { .. } => fatal_transport(
            "mcp_transport_ca_cert_invalid",
            top.to_string(),
            "MCP HTTP transport extra CA certificate was invalid",
        ),
        HttpTransportBuildError::Client(_) => fatal_transport(
            "mcp_transport_client_build_failed",
            top.to_string(),
            "MCP HTTP transport reqwest client build failed",
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
        ServiceError::TransportSend(_) => classify_transport_send_error(top, err),
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
        _ => fatal_transport(
            "mcp_service_error",
            top.to_string(),
            "MCP service returned an unclassified error",
        ),
    }
}

fn classify_transport_send_error(top: &ConnectionError, err: &ServiceError) -> ToolSessionError {
    match classify_streamable_http_error(err) {
        Some(HttpTransportFailure::SessionExpired) => classified_transport(
            "mcp_session_expired",
            ErrorDisposition::InformAndContinue,
            top.to_string(),
            Some(
                "MCP HTTP session expired; next resolve rebuilds connection, but this call is not replayed",
            ),
        ),
        Some(HttpTransportFailure::AuthRequired) => classified_transport(
            "mcp_auth_required",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP HTTP server requires authentication; check configured credentials"),
        ),
        Some(HttpTransportFailure::InsufficientScope) => classified_transport(
            "mcp_insufficient_scope",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP HTTP credentials lack required scope; update secret or authorization policy"),
        ),
        Some(HttpTransportFailure::Timeout) => classified_transport(
            "mcp_network_timeout",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP HTTP transport timed out"),
        ),
        Some(HttpTransportFailure::Connect) => classified_transport(
            "mcp_network_connect_failed",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP HTTP transport could not connect to server"),
        ),
        Some(HttpTransportFailure::PolicyOrConfig) => classified_transport(
            "mcp_transport_config_failed",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP HTTP transport configuration was rejected"),
        ),
        Some(HttpTransportFailure::Protocol) => classified_transport(
            "mcp_protocol_error",
            ErrorDisposition::Fatal,
            top.to_string(),
            Some("MCP HTTP server returned an invalid or unsupported protocol response"),
        ),
        Some(HttpTransportFailure::Network) | None => classified_transport(
            "mcp_transport_send_failed",
            ErrorDisposition::HostRetriable,
            top.to_string(),
            Some("MCP transport failed while sending request"),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpTransportFailure {
    SessionExpired,
    AuthRequired,
    InsufficientScope,
    Timeout,
    Connect,
    PolicyOrConfig,
    Protocol,
    Network,
}

fn classify_streamable_http_error(err: &ServiceError) -> Option<HttpTransportFailure> {
    let ServiceError::TransportSend(transport_err) = err else {
        return None;
    };

    // rmcp preserves the concrete transport error only on request-path
    // `ServiceError::TransportSend` failures (e.g. our `send_cancellable_request`
    // tools/call path). Startup/initialize errors are currently stringified
    // before they reach this mapper, so they cannot be classified here.
    // Future transports may box different error types; non-matching downcasts
    // intentionally fall back to the generic transport classification.
    let mut source: Option<&(dyn StdError + 'static)> = Some(transport_err.error.as_ref());
    while let Some(err) = source {
        if let Some(http_err) = err.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            return Some(classify_streamable_http_variant(http_err));
        }
        source = err.source();
    }
    None
}

fn classify_streamable_http_variant(
    err: &StreamableHttpError<reqwest::Error>,
) -> HttpTransportFailure {
    match err {
        StreamableHttpError::SessionExpired => HttpTransportFailure::SessionExpired,
        StreamableHttpError::AuthRequired(_) => HttpTransportFailure::AuthRequired,
        StreamableHttpError::InsufficientScope(_) => HttpTransportFailure::InsufficientScope,
        StreamableHttpError::Client(err) if err.is_timeout() => HttpTransportFailure::Timeout,
        StreamableHttpError::Client(err) if err.is_connect() => HttpTransportFailure::Connect,
        StreamableHttpError::ReservedHeaderConflict(_) => HttpTransportFailure::PolicyOrConfig,
        StreamableHttpError::UnexpectedServerResponse(_)
        | StreamableHttpError::UnexpectedContentType(_)
        | StreamableHttpError::ServerDoesNotSupportSse
        | StreamableHttpError::ServerDoesNotSupportDeleteSession
        | StreamableHttpError::Deserialize(_)
        | StreamableHttpError::MissingSessionIdInResponse => HttpTransportFailure::Protocol,
        StreamableHttpError::Sse(_)
        | StreamableHttpError::Io(_)
        | StreamableHttpError::Client(_)
        | StreamableHttpError::UnexpectedEndOfStream
        | StreamableHttpError::TokioJoinError(_)
        | StreamableHttpError::TransportChannelClosed => HttpTransportFailure::Network,
        // rmcp marks this enum `#[non_exhaustive]`; new variants fail safe as network errors
        // until classified explicitly above.
        _ => HttpTransportFailure::Network,
    }
}

fn fatal_transport(code: &'static str, message: String, hint: &'static str) -> ToolSessionError {
    classified_transport(code, ErrorDisposition::Fatal, message, Some(hint))
}

fn retriable_transport(
    code: &'static str,
    message: String,
    hint: &'static str,
) -> ToolSessionError {
    classified_transport(code, ErrorDisposition::HostRetriable, message, Some(hint))
}

fn inform_transport(code: &'static str, message: String, hint: &'static str) -> ToolSessionError {
    classified_transport(
        code,
        ErrorDisposition::InformAndContinue,
        message,
        Some(hint),
    )
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
/// The shape mirrors the `ContentEnvelope` output mode used by generated MCP tools.
fn result_to_envelope(result: CallToolResult) -> Value {
    let mut content = Vec::with_capacity(result.content.len());
    let mut serialization_failed = false;
    for block in &result.content {
        match serde_json::to_value(block.raw.clone()) {
            Ok(value) => content.push(value),
            Err(err) => {
                serialization_failed = true;
                tracing::warn!(error = %err, "failed to serialize MCP content block");
            }
        }
    }
    let structured = match result.structured_content {
        Some(sc) => match serde_json::to_value(sc) {
            Ok(value) => value,
            Err(err) => {
                serialization_failed = true;
                tracing::warn!(error = %err, "failed to serialize MCP structured content");
                Value::Null
            }
        },
        None => Value::Null,
    };
    json!({
        "content": content,
        "structured": structured,
        "is_error": result.is_error.unwrap_or(false) || serialization_failed,
        "metadata": Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    use rmcp::transport::{
        DynamicTransportError,
        streamable_http_client::{AuthRequiredError, InsufficientScopeError},
    };

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

    fn transport_send(http_err: StreamableHttpError<reqwest::Error>) -> ServiceError {
        ServiceError::TransportSend(DynamicTransportError::from_parts(
            "streamable_http",
            TypeId::of::<()>(),
            Box::new(http_err),
        ))
    }

    #[test]
    fn auth_required_transport_error_maps_to_permanent_auth_code() {
        let top = ConnectionError::CallTool(ServiceError::TransportClosed);
        let err = transport_send(StreamableHttpError::AuthRequired(AuthRequiredError::new(
            "Bearer".into(),
        )));
        let classified = classified(classify_service_error(&top, &err));
        assert_eq!(classified.code, "mcp_auth_required");
        assert_eq!(classified.disposition, ErrorDisposition::Fatal);
    }

    #[test]
    fn insufficient_scope_transport_error_maps_to_permanent_auth_code() {
        let top = ConnectionError::CallTool(ServiceError::TransportClosed);
        let err = transport_send(StreamableHttpError::InsufficientScope(
            InsufficientScopeError::new("Bearer scope=admin".into(), Some("admin".into())),
        ));
        let classified = classified(classify_service_error(&top, &err));
        assert_eq!(classified.code, "mcp_insufficient_scope");
        assert_eq!(classified.disposition, ErrorDisposition::Fatal);
    }

    #[test]
    fn digest_mismatch_maps_to_contract_violation_not_invalid_argument() {
        let err = connection_error_to_session(ConnectionError::ToolsDigestMismatch {
            server_id: "remote".into(),
            expected: baml_rt_tools::mcp_snapshot::Digest::new(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            observed: baml_rt_tools::mcp_snapshot::Digest::new(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        });
        let classified = classified(err);
        assert_eq!(classified.code, "mcp_contract_violation");
        assert_eq!(classified.disposition, ErrorDisposition::Fatal);
    }
}
