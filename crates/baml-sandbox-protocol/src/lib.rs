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

pub use codec::{CodecError, MAX_FRAME_BYTES, TsrpcChannel};
pub use protocol::{
    ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, ErrorClass, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, PROTOCOL_VERSION,
    SUPPORTED_METHODS, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
