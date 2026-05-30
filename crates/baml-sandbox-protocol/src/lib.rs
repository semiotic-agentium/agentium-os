// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared wire contract for the sandbox tool protocol (TSRPC).
//!
//! This crate is the single source of truth for types exchanged across the
//! sandbox host/guest boundary:
//!
//! - [`protocol`] — JSON-RPC 2.0 envelopes plus method names, error codes,
//!   and payload shapes (`tool/describe`, `tool/invoke`).
//! - [`codec`] — [`TsrpcChannel`], a length-prefixed JSON frame codec layered
//!   over any `AsyncRead + AsyncWrite`.
//!
//! Both sides of the sandbox boundary depend on this crate directly so the
//! wire format cannot drift between host and guest. Guests ship in distroless
//! images; this crate therefore keeps its dep surface narrow
//! (`serde`/`serde_json`/`tokio`/`thiserror` only) and avoids host-only
//! abstractions like `tracing` or `baml-rt-core` error types.

pub mod codec;
pub mod protocol;
pub mod session;

pub use codec::{CodecError, MAX_FRAME_BYTES, TsrpcChannel};
pub use protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR,
    ERR_PAYLOAD_LIMIT_EXCEEDED, ERR_SCHEMA_DIGEST_MISMATCH, ERR_SIDECAR_MALFORMED,
    ERR_SIDECAR_MISSING, ERR_SIDECAR_SCHEMA_INVALID, ERR_SIDECAR_SIZE_EXCEEDED,
    ERR_UNSUPPORTED_PROTOCOL, ErrorClass, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, PROTOCOL_VERSION, SUPPORTED_METHODS,
    SUPPORTED_METHODS_V2, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult, ToolSchemaResult,
};
pub use session::{
    METHOD_SESSION_ABORT, METHOD_SESSION_FINISH, METHOD_SESSION_OPEN, METHOD_SESSION_READ,
    METHOD_SESSION_RESET, METHOD_SESSION_SEND, SUPPORTED_METHODS_SESSION, SessionAbortParams,
    SessionAbortResult, SessionDisposition, SessionFinishParams, SessionFinishResult,
    SessionOpenParams, SessionOpenResult, SessionReadParams, SessionReadResult,
    SessionResetOutcome, SessionResetParams, SessionResetResult, SessionSendParams,
    SessionSendResult, StepEnvelope, StepError, error_code,
};
