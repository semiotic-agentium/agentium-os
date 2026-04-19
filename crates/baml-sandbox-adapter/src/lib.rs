//! Guest-side SDK for sandbox tool adapters.
//!
//! The adapter SDK layers above microsandbox (a byte-stream transport) and
//! provides the application-level contract tool authors implement:
//!
//! - [`SandboxTool`] — author-facing trait; authors return a domain
//!   [`AdapterError`], never a `JsonRpcError`. The runtime performs the
//!   mapping to wire errors centrally.
//! - [`run_adapter`] — single `Result`-returning entry point a binary's
//!   `main` wraps. Owns the stdio framing loop, panic containment, stdout
//!   purity, and per-request error isolation.
//!
//! The wire contract itself lives in [`baml_sandbox_protocol`]; this crate
//! is deliberately thin so it can ship in distroless guest images without
//! pulling host-side observability or error taxonomy.

#[cfg(not(unix))]
compile_error!(
    "baml-sandbox-adapter requires a Unix target (uses dup/dup2 + FD_CLOEXEC to isolate stdout from framed output)"
);

mod stdout_swap;

use async_trait::async_trait;
use baml_sandbox_protocol::{
    ERR_INTERNAL, ErrorClass, JsonRpcError, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
use serde_json::json;

/// Boxed opaque source error used by [`AdapterError`] transient/execution
/// variants. Kept behind a type alias so author code can `.into()` most
/// error types directly.
pub type BoxedErr = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Domain error surface exposed to tool authors.
///
/// Authors return this from [`SandboxTool::invoke`]; the adapter runtime
/// maps each variant onto a [`JsonRpcError`] wire frame via
/// [`AdapterError::into_json_rpc`], so author code never has to think about
/// JSON-RPC error codes or the `error_class` data field.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("transient failure: {message}")]
    Transient {
        message: String,
        #[source]
        source: Option<BoxedErr>,
    },
    #[error("execution failure: {message}")]
    Execution {
        message: String,
        #[source]
        source: Option<BoxedErr>,
    },
}

impl AdapterError {
    /// Machine-readable classification carried on the wire as
    /// `error.data.error_class`.
    pub fn error_class(&self) -> ErrorClass {
        match self {
            AdapterError::InvalidArgument(_) => ErrorClass::InvalidArgument,
            AdapterError::Configuration(_) => ErrorClass::Configuration,
            AdapterError::Permission(_) => ErrorClass::Permission,
            AdapterError::Transient { .. } => ErrorClass::Transient,
            AdapterError::Execution { .. } => ErrorClass::Execution,
        }
    }

    /// Central mapping from the domain error surface onto a wire-level
    /// [`JsonRpcError`]. All adapter-originated errors share the
    /// application-defined [`ERR_INTERNAL`] code; dispatch-layer errors
    /// (parse, unknown method) set their own codes and never flow through
    /// here.
    // Reserved for the slice-3 dispatch loop; tests exercise it today.
    #[allow(dead_code)]
    pub(crate) fn into_json_rpc(self) -> JsonRpcError {
        let class = self.error_class();
        let message = self.to_string();
        let source_chain = collect_source_chain(&self);
        let mut data = json!({ "error_class": class });
        if !source_chain.is_empty() {
            data["source"] = json!(source_chain);
        }
        JsonRpcError {
            code: ERR_INTERNAL,
            message,
            data: Some(data),
        }
    }
}

// Reserved for the slice-3 dispatch loop; reached via into_json_rpc.
#[allow(dead_code)]
fn collect_source_chain(err: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = err.source();
    while let Some(next) = current {
        chain.push(next.to_string());
        current = next.source();
    }
    chain
}

/// Author-facing trait implemented by sandboxed tools.
///
/// `describe` is synchronous because it returns static metadata; `invoke`
/// is async so authors can perform I/O.
#[async_trait]
pub trait SandboxTool: Send + Sync {
    fn describe(&self) -> ToolDescribeResult;
    async fn invoke(&self, params: ToolInvokeParams) -> Result<ToolInvokeResult, AdapterError>;
}

/// Adapter entry point. A binary's `main` is expected to construct the
/// tool, call `run_adapter(tool).await`, and translate any returned error
/// into stderr + non-zero exit.
///
/// The real dispatch loop (stdout-fd swap, frame read/write, panic
/// containment, per-request error isolation, explicit writer shutdown)
/// lands in subsequent slices of the X4.2 workstream. This slice-1 stub
/// exists so the crate compiles and downstream layout (echo binary,
/// stdio E2E tests) can be wired in without a second churn pass on the
/// public surface.
pub async fn run_adapter<T: SandboxTool>(_tool: T) -> Result<(), AdapterError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_class_maps_each_variant() {
        assert_eq!(
            AdapterError::InvalidArgument("x".into()).error_class(),
            ErrorClass::InvalidArgument
        );
        assert_eq!(
            AdapterError::Configuration("x".into()).error_class(),
            ErrorClass::Configuration
        );
        assert_eq!(
            AdapterError::Permission("x".into()).error_class(),
            ErrorClass::Permission
        );
        assert_eq!(
            AdapterError::Transient {
                message: "x".into(),
                source: None,
            }
            .error_class(),
            ErrorClass::Transient
        );
        assert_eq!(
            AdapterError::Execution {
                message: "x".into(),
                source: None,
            }
            .error_class(),
            ErrorClass::Execution
        );
    }

    #[test]
    fn into_json_rpc_sets_internal_code_and_error_class() {
        let err = AdapterError::InvalidArgument("bad input".into());
        let rpc = err.into_json_rpc();
        assert_eq!(rpc.code, ERR_INTERNAL);
        assert!(rpc.message.contains("bad input"));
        let data = rpc.data.expect("data present");
        assert_eq!(data["error_class"], "invalid_argument");
        assert!(data.get("source").is_none());
    }

    #[test]
    fn into_json_rpc_includes_source_chain_when_present() {
        let inner: BoxedErr = "boom".into();
        let err = AdapterError::Transient {
            message: "upstream flaked".into(),
            source: Some(inner),
        };
        let rpc = err.into_json_rpc();
        let data = rpc.data.expect("data present");
        assert_eq!(data["error_class"], "transient");
        assert_eq!(data["source"], json!(["boom"]));
    }
}
