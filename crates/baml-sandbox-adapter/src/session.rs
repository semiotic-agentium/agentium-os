//! Session-mode adapter SDK (Phase 1 skeleton).
//!
//! Defines the author-facing [`SandboxSessionTool`] trait and the dispatch
//! stubs that map `tool/session_*` JSON-RPC frames onto trait methods. The
//! runtime/sequencing pieces called out in `plans/sandbox_streaming.md` §7
//! (session table, single-reader enforcement, reset gating) land in later
//! phases.
//!
//! Wire types live in [`baml_sandbox_protocol::session`]; this module only
//! adds the application-layer trait, error mapping, and dispatch glue.

use async_trait::async_trait;
use baml_sandbox_protocol::{
    self as proto, ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, ErrorClass, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse,
    session::{
        SessionAbortParams, SessionAbortResult, SessionFinishParams, SessionFinishResult,
        SessionOpenParams, SessionOpenResult, SessionReadParams, SessionReadResult,
        SessionResetOutcome, SessionResetParams, SessionResetResult, SessionSendParams,
        SessionSendResult, StepEnvelope,
    },
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::AdapterError;

/// Outcome of [`SandboxSessionTool::on_reset`]. Drives the host-side reuse gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// Sandbox state was successfully cleared and may be checked back into the pool.
    Ok,
    /// Adapter does not implement reset; sandbox MUST be destroyed after finish.
    Unsupported,
}

impl From<ResetOutcome> for SessionResetOutcome {
    fn from(value: ResetOutcome) -> Self {
        match value {
            ResetOutcome::Ok => SessionResetOutcome::Ok,
            ResetOutcome::Unsupported => SessionResetOutcome::Unsupported,
        }
    }
}

/// Author-facing trait implemented by sandboxed session tools.
///
/// Compared to the single-shot [`crate::SandboxTool`] trait, every method
/// takes an explicit `session_id` so the trait owner can multiplex multiple
/// sessions inside one process. The Phase 3 host enforces the
/// "one-live-session-per-sandbox" invariant; this trait does not preclude
/// authors from holding additional state per session if useful (e.g. test
/// fixtures).
///
/// Errors split along two axes:
/// - [`AdapterError`] returned from a method = transport-level failure, mapped
///   onto a [`JsonRpcError`] frame.
/// - [`StepEnvelope::Error`] returned from `read` = in-stream classified error
///   surfaced to the LLM/host policy via
///   [`baml_sandbox_protocol::session::SessionDisposition`].
#[async_trait]
pub trait SandboxSessionTool: Send + Sync {
    async fn open(&self, params: SessionOpenParams) -> Result<SessionOpenResult, AdapterError>;
    async fn send(&self, params: SessionSendParams) -> Result<SessionSendResult, AdapterError>;
    async fn read(&self, params: SessionReadParams) -> Result<StepEnvelope, AdapterError>;
    async fn finish(
        &self,
        params: SessionFinishParams,
    ) -> Result<SessionFinishResult, AdapterError>;
    async fn abort(&self, params: SessionAbortParams) -> Result<SessionAbortResult, AdapterError>;

    /// Default implementation reports the adapter does not support reuse.
    /// Tools that want pool reuse must override.
    async fn on_reset(&self) -> Result<ResetOutcome, AdapterError> {
        Ok(ResetOutcome::Unsupported)
    }
}

/// Dispatch a single `tool/session_*` JSON-RPC request onto the trait.
///
/// Phase 1 stub: no session-table validation, no single-reader enforcement,
/// no resume-token policing. Those land in Phase 3 alongside the host-side
/// pool/channel lifecycle. The shape is stable so later phases can layer
/// validation around it without churning callers.
pub async fn dispatch_session_request<T: SandboxSessionTool>(
    tool: &T,
    req: JsonRpcRequest,
) -> JsonRpcResponse {
    let id = req.id;
    let method = req.method.clone();
    match method.as_str() {
        proto::METHOD_SESSION_OPEN => match parse::<SessionOpenParams>(&method, req.params) {
            Ok(p) => to_response(id, tool.open(p).await),
            Err(resp) => resp.with_id(id),
        },
        proto::METHOD_SESSION_SEND => match parse::<SessionSendParams>(&method, req.params) {
            Ok(p) => to_response(id, tool.send(p).await),
            Err(resp) => resp.with_id(id),
        },
        proto::METHOD_SESSION_READ => match parse::<SessionReadParams>(&method, req.params) {
            Ok(p) => {
                // `SessionReadResult` is a type alias for `StepEnvelope`.
                let result: Result<SessionReadResult, AdapterError> = tool.read(p).await;
                to_response(id, result)
            }
            Err(resp) => resp.with_id(id),
        },
        proto::METHOD_SESSION_FINISH => match parse::<SessionFinishParams>(&method, req.params) {
            Ok(p) => to_response(id, tool.finish(p).await),
            Err(resp) => resp.with_id(id),
        },
        proto::METHOD_SESSION_ABORT => match parse::<SessionAbortParams>(&method, req.params) {
            Ok(p) => to_response(id, tool.abort(p).await),
            Err(resp) => resp.with_id(id),
        },
        proto::METHOD_SESSION_RESET => match parse::<SessionResetParams>(&method, req.params) {
            Ok(_p) => {
                let outcome = tool
                    .on_reset()
                    .await
                    .map(|o| SessionResetResult { outcome: o.into() });
                to_response(id, outcome)
            }
            Err(resp) => resp.with_id(id),
        },
        other => unknown_method(id, other),
    }
}

struct PendingResponse {
    code: i32,
    message: String,
    class: ErrorClass,
}

impl PendingResponse {
    fn with_id(self, id: u64) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: self.code,
                message: self.message,
                data: Some(json!({ "error_class": self.class })),
            }),
        }
    }
}

fn parse<P: serde::de::DeserializeOwned>(
    method: &str,
    params: Value,
) -> Result<P, PendingResponse> {
    serde_json::from_value(params).map_err(|err| PendingResponse {
        code: ERR_PARSE_ERROR,
        message: format!("invalid {method} params: {err}"),
        class: ErrorClass::InvalidArgument,
    })
}

fn to_response<R: Serialize>(id: u64, outcome: Result<R, AdapterError>) -> JsonRpcResponse {
    match outcome {
        Ok(value) => match serde_json::to_value(&value) {
            Ok(json) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(json),
                error: None,
            },
            Err(err) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: ERR_INTERNAL,
                    message: format!("failed to serialize session response: {err}"),
                    data: Some(json!({ "error_class": ErrorClass::Execution })),
                }),
            },
        },
        Err(adapter_err) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(adapter_err.into_json_rpc()),
        },
    }
}

fn unknown_method(id: u64, method: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code: ERR_METHOD_NOT_FOUND,
            message: format!("unknown session method '{method}'"),
            data: Some(json!({ "error_class": ErrorClass::InvalidArgument })),
        }),
    }
}

#[cfg(test)]
mod tests {
    use baml_sandbox_protocol::session::{SessionDisposition, StepError, error_code};
    use serde_json::json;

    use super::*;

    struct StubTool;

    #[async_trait]
    impl SandboxSessionTool for StubTool {
        async fn open(
            &self,
            _params: SessionOpenParams,
        ) -> Result<SessionOpenResult, AdapterError> {
            Ok(SessionOpenResult {
                session_id: "s-1".into(),
                initial_step: None,
            })
        }
        async fn send(
            &self,
            _params: SessionSendParams,
        ) -> Result<SessionSendResult, AdapterError> {
            Ok(SessionSendResult::default())
        }
        async fn read(&self, _params: SessionReadParams) -> Result<StepEnvelope, AdapterError> {
            Ok(StepEnvelope::Error {
                error: StepError {
                    code: error_code::CHUNK_TIMEOUT.into(),
                    message: "timed out".into(),
                    disposition: SessionDisposition::InformAndContinue,
                    hint: None,
                    retry_after_ms: None,
                },
            })
        }
        async fn finish(
            &self,
            _params: SessionFinishParams,
        ) -> Result<SessionFinishResult, AdapterError> {
            Ok(SessionFinishResult::default())
        }
        async fn abort(
            &self,
            _params: SessionAbortParams,
        ) -> Result<SessionAbortResult, AdapterError> {
            Ok(SessionAbortResult::default())
        }
    }

    /// Session_open dispatches and `on_reset` defaults to `Unsupported`,
    /// covering the success path + the trait default in one go.
    #[tokio::test]
    async fn dispatch_open_succeeds_and_default_reset_is_unsupported() {
        let req = JsonRpcRequest::new(
            proto::METHOD_SESSION_OPEN,
            7,
            json!({
                "invocation_id": "inv-1",
                "tool_name": "demo",
                "open_input": {}
            }),
        );
        let resp = dispatch_session_request(&StubTool, req).await;
        assert!(
            resp.error.is_none(),
            "open should succeed: {:?}",
            resp.error
        );
        let result = resp.result.expect("result present");
        assert_eq!(result["session_id"], "s-1");

        assert_eq!(
            StubTool.on_reset().await.unwrap(),
            ResetOutcome::Unsupported
        );
    }

    /// session_read can carry an in-stream error envelope; an unknown
    /// session method yields `ERR_METHOD_NOT_FOUND`.
    #[tokio::test]
    async fn read_returns_step_envelope_and_unknown_method_is_method_not_found() {
        let read_req =
            JsonRpcRequest::new(proto::METHOD_SESSION_READ, 8, json!({"session_id": "s-1"}));
        let resp = dispatch_session_request(&StubTool, read_req).await;
        let result = resp.result.expect("result present");
        assert_eq!(result["step"], "error");
        assert_eq!(result["error"]["code"], error_code::CHUNK_TIMEOUT);

        let unknown_req = JsonRpcRequest::new("tool/session_bogus", 9, json!({}));
        let resp = dispatch_session_request(&StubTool, unknown_req).await;
        assert_eq!(resp.error.unwrap().code, ERR_METHOD_NOT_FOUND);
    }
}
