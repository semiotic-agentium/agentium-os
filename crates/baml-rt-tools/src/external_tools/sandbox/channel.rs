//! Length-prefixed JSON frame codec for the sandbox workload transport.
//!
//! The codec itself lives in the [`baml_sandbox_protocol`] crate so host and
//! guest adapters share a single source of truth. This module re-exports it
//! under the historical path
//! `baml_rt_tools::external_tools::sandbox::channel::{TsrpcChannel,
//! MAX_FRAME_BYTES}` so existing call sites keep compiling unchanged.
//!
//! Callers that need to convert [`CodecError`] into
//! [`baml_rt_core::BamlRtError`] do so at the call site (see
//! `sandbox::invoker`) — the protocol crate deliberately stays ignorant of
//! host error taxonomies.

pub use baml_sandbox_protocol::{CodecError, MAX_FRAME_BYTES, TsrpcChannel};
