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

mod panic_catch;
mod stdout_swap;

use async_trait::async_trait;
use baml_sandbox_protocol::{
    CodecError, ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, ErrorClass, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, ToolDescribeResult,
    ToolInvokeParams, ToolInvokeResult, TsrpcChannel,
};
use serde_json::{Value, json};

use crate::panic_catch::catch_tool_panic;

/// Boxed opaque source error used by [`AdapterError`] transient/execution
/// variants. Kept behind a type alias so author code can `.into()` most
/// error types directly.
pub type BoxedErr = Box<dyn std::error::Error + Send + Sync + 'static>;

/// JSON-RPC response id emitted when the inbound frame's id cannot be
/// recovered (malformed JSON, wrong envelope shape). The wire contract
/// requires a numeric id, and `0` is reserved as a sentinel so hosts can
/// distinguish "this was a parse error where the real id is unknown"
/// from a well-formed response. Real invocations must avoid `0` as an id.
pub const UNKNOWN_REQUEST_ID: u64 = 0;

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
/// Lifecycle:
/// 1. Install the stdout-fd swap so any `println!` in user code lands on
///    stderr and cannot corrupt framed output.
/// 2. Build a [`TsrpcChannel`] over stdin + the dup'd original stdout.
/// 3. Run the dispatch loop — clean exit on EOF, per-request error frame
///    on recoverable failures, [`AdapterError::Execution`] on wire
///    desync.
/// 4. Explicit `shutdown().await` on the channel so the last response
///    frame reaches the host before the process exits.
pub async fn run_adapter<T: SandboxTool>(tool: T) -> Result<(), AdapterError> {
    let handle = stdout_swap::install_stdout_swap().map_err(|source| AdapterError::Execution {
        message: format!("stdout swap failed: {source}"),
        source: Some(Box::new(source)),
    })?;
    let writer = handle.into_async_writer();
    let stdin = tokio::io::stdin();
    let mut channel = TsrpcChannel::new(stdin, writer);

    let loop_result = dispatch_loop(&tool, &mut channel).await;

    if let Err(shutdown_err) = channel.shutdown().await {
        // Shutdown after an already-failed loop (peer gone) is expected
        // to fail with a broken pipe; a post-EOF shutdown may similarly
        // be a no-op. Surface via tracing, never escalate — the loop's
        // result is the authoritative outcome.
        tracing::debug!(error = %shutdown_err, "TsrpcChannel shutdown after dispatch loop");
    }
    loop_result
}

async fn dispatch_loop<T: SandboxTool>(
    tool: &T,
    channel: &mut TsrpcChannel,
) -> Result<(), AdapterError> {
    loop {
        let raw = match channel.recv().await {
            Ok(value) => value,
            Err(err) => match classify_recv_error(err) {
                RecvFate::Eof => return Ok(()),
                RecvFate::WireDesync(fatal) => return Err(fatal),
                RecvFate::ParseError(msg) => {
                    let response = error_response(
                        UNKNOWN_REQUEST_ID,
                        ERR_PARSE_ERROR,
                        msg,
                        ErrorClass::InvalidArgument,
                    );
                    send_or_terminate(channel, &response).await?;
                    continue;
                }
            },
        };

        let request = match serde_json::from_value::<JsonRpcRequest>(raw) {
            Ok(r) => r,
            Err(err) => {
                let response = error_response(
                    UNKNOWN_REQUEST_ID,
                    ERR_PARSE_ERROR,
                    format!("invalid JSON-RPC request envelope: {err}"),
                    ErrorClass::InvalidArgument,
                );
                send_or_terminate(channel, &response).await?;
                continue;
            }
        };

        if request.jsonrpc != "2.0" {
            let response = error_response(
                request.id,
                ERR_INTERNAL,
                format!(
                    "unsupported JSON-RPC envelope version '{}': adapter requires '2.0'",
                    request.jsonrpc
                ),
                ErrorClass::InvalidArgument,
            );
            send_or_terminate(channel, &response).await?;
            continue;
        }

        let response = dispatch_one(tool, request).await;
        send_or_terminate(channel, &response).await?;
    }
}

/// Classification of a `recv` failure into loop-fate categories.
enum RecvFate {
    /// Peer closed the wire cleanly — expected teardown, exit 0.
    Eof,
    /// Wire irrecoverably desynced — return error, `main` exits 1.
    WireDesync(AdapterError),
    /// Frame parse failed but the wire is still in sync — emit a
    /// per-request error frame and keep serving.
    ParseError(String),
}

fn classify_recv_error(err: CodecError) -> RecvFate {
    match err {
        CodecError::Io { op, source } => {
            if source.kind() == std::io::ErrorKind::UnexpectedEof {
                RecvFate::Eof
            } else {
                RecvFate::WireDesync(AdapterError::Execution {
                    message: format!("stdin {op} failed: {source}"),
                    source: Some(Box::new(source)),
                })
            }
        }
        CodecError::FrameTooLarge { len, max } => RecvFate::WireDesync(AdapterError::Execution {
            message: format!("inbound frame {len} bytes exceeds MAX_FRAME_BYTES {max}"),
            source: None,
        }),
        CodecError::Deserialize { source } => {
            RecvFate::ParseError(format!("invalid JSON frame: {source}"))
        }
        // `Serialize` cannot surface on recv; match exhaustively and
        // downgrade rather than panic if the codec ever changes.
        CodecError::Serialize { source } => {
            RecvFate::ParseError(format!("unexpected serialize error on recv: {source}"))
        }
    }
}

async fn dispatch_one<T: SandboxTool>(tool: &T, req: JsonRpcRequest) -> JsonRpcResponse {
    if req.method == METHOD_DESCRIBE {
        match serde_json::to_value(tool.describe()) {
            Ok(value) => success_response(req.id, value),
            Err(err) => error_response(
                req.id,
                ERR_INTERNAL,
                format!("failed to serialize describe result: {err}"),
                ErrorClass::Execution,
            ),
        }
    } else if req.method == METHOD_INVOKE {
        let params: ToolInvokeParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(err) => {
                return error_response(
                    req.id,
                    ERR_PARSE_ERROR,
                    format!("invalid tool/invoke params: {err}"),
                    ErrorClass::InvalidArgument,
                );
            }
        };
        match catch_tool_panic(tool.invoke(params)).await {
            Ok(Ok(result)) => match serde_json::to_value(result) {
                Ok(value) => success_response(req.id, value),
                Err(err) => error_response(
                    req.id,
                    ERR_INTERNAL,
                    format!("failed to serialize invoke result: {err}"),
                    ErrorClass::Execution,
                ),
            },
            Ok(Err(adapter_err)) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(adapter_err.into_json_rpc()),
            },
            Err(panic_msg) => error_response(
                req.id,
                ERR_INTERNAL,
                format!("tool panicked: {panic_msg}"),
                ErrorClass::Execution,
            ),
        }
    } else {
        error_response(
            req.id,
            ERR_METHOD_NOT_FOUND,
            format!("unknown method '{}'", req.method),
            ErrorClass::InvalidArgument,
        )
    }
}

fn success_response(id: u64, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: u64, code: i32, message: String, class: ErrorClass) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data: Some(json!({ "error_class": class })),
        }),
    }
}

/// Send a response frame; a send failure terminates the loop because it
/// indicates the host wire is no longer usable (broken pipe, closed peer).
async fn send_or_terminate(
    channel: &mut TsrpcChannel,
    response: &JsonRpcResponse,
) -> Result<(), AdapterError> {
    let value = match serde_json::to_value(response) {
        Ok(v) => v,
        Err(err) => {
            return Err(AdapterError::Execution {
                message: format!("failed to serialize response frame: {err}"),
                source: Some(Box::new(err)),
            });
        }
    };
    channel
        .send(&value)
        .await
        .map_err(|err| AdapterError::Execution {
            message: format!("failed to send response frame: {err}"),
            source: Some(Box::new(err)),
        })
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
