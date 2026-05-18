//! Long-lived MCP client connection used by the runtime tool handler.
//!
//! Each `McpConnection` owns a single server-process subscription. The first
//! `call_tool` on the connection spawns the child, establishes the rmcp
//! `RunningService`, and caches it; subsequent calls reuse it. The connection
//! is shared across every tool whose snapshot resolves to the same server,
//! so an agent that uses multiple Grafana MCP tools sees one child process.

use std::{
    collections::BTreeMap,
    error::Error as StdError,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use baml_rt_observability::{record_mcp_digest_mismatch, record_mcp_session_expired};
use baml_rt_tools::{
    mcp_cache::mark_server_stale,
    mcp_config::{HttpHeader as ConfigHttpHeader, HttpNetworkPolicyConfig},
    mcp_schema_normalize::digest_input_schema,
    mcp_secrets::ResolvedSecret,
    mcp_snapshot::{compute_server_identity_digest, compute_tools_digest_from_entries},
};
use rmcp::{
    ErrorData as McpError, ServiceExt,
    handler::client::ClientHandler,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotification,
        CancelledNotificationParam, ClientRequest, CreateElicitationRequestParams,
        CreateElicitationResult, CreateMessageRequestMethod, CreateMessageRequestParams,
        CreateMessageResult, ErrorCode, ListRootsRequestMethod, ListRootsResult,
        ProgressNotificationParam, RequestId, ServerResult,
    },
    service::{
        MaybeSendFuture, NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleClient,
        RunningService, ServiceError,
    },
    transport::{
        ConfigureCommandExt, TokioChildProcess, streamable_http_client::StreamableHttpError,
    },
};
use serde_json::{Value, to_value};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, OnceCell},
};
use tokio_util::sync::CancellationToken;

use crate::http::transport::{HttpTransportBuildError, build_rmcp_http_transport};

/// Maximum time we wait for `initialize + tools/list` during lazy connection
/// startup. Caps a misbehaving server's hold on the calling task.
const STARTUP_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);
/// Default per-call timeout used when the operator config does not specify one.
const RUNTIME_CALL_TIMEOUT_DEFAULT: Duration = Duration::from_secs(120);
// Best-effort bound for abort-time `notifications/cancelled`.
//
// Verified against rmcp 1.7.0: `StreamableHttpClientWorker::run` processes
// outbound `Event::ClientMessage` items serially — it awaits
// `client.post_message(...)` for the current event before reading the next.
// A cancellation notification sent through the same `Peer` therefore queues
// behind the in-flight `tools/call` POST and will not deliver until the
// original request completes. This timeout protects platform cancellation
// from waiting on server completion; on HTTP transports the wire
// notification is best-effort and typically arrives only after the call
// would have finished anyway. Other transports may deliver more promptly
// depending on their writer model. Local termination of the call future is
// unaffected and remains immediate via the `local_token` cancellation path.
const ABORT_CANCEL_NOTIFY_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("failed to spawn MCP server `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("transport setup failed: {0}")]
    Transport(String),
    #[error("initialize timed out after {0:?}")]
    InitializeTimeout(Duration),
    #[error("initialize failed: {0}")]
    InitializeFailed(String),
    #[error("call_tool failed: {0}")]
    CallTool(#[from] ServiceError),
    #[error("MCP server `{server_id}` HTTP session expired; registry will rebuild on next resolve")]
    SessionExpired { server_id: String },
    #[error("MCP arguments must be a JSON object, got {0}")]
    InvalidArguments(String),
    #[error("MCP call cancelled for server `{server_id}`: {reason}")]
    CallCancelled { server_id: String, reason: String },
    #[error("MCP call timed out after {0:?}")]
    CallTimeout(Duration),
    #[error(
        "MCP server `{server_id}` is stale (startup tools/list or tools/list_changed observed drift); operator must re-import and approve a new registry snapshot"
    )]
    Stale { server_id: String },
    #[error(
        "MCP server `{server_id}` identity digest mismatch (expected `{expected}`, observed `{observed}`); operator must re-import and approve a new registry snapshot"
    )]
    IdentityMismatch {
        server_id: String,
        expected: String,
        observed: String,
    },
    #[error(
        "MCP server `{server_id}` tool surface digest mismatch (expected `{expected}`, observed `{observed}`); operator must re-import and approve a new registry snapshot"
    )]
    ToolsDigestMismatch {
        server_id: String,
        expected: String,
        observed: String,
    },
    #[error("MCP server `{server_id}` startup tools/list failed: {reason}")]
    StartupToolsListFailed { server_id: String, reason: String },
    #[error("MCP server `{server_id}` did not return peer_info after initialize")]
    MissingPeerInfo { server_id: String },
    #[error("MCP server `{server_id}` peer_info failed to serialize for identity digest: {source}")]
    IdentitySerializeFailed {
        server_id: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Spawn parameters frozen at registration time. The runtime never reads MCP
/// config from disk during a tool call; the resolver builds one of these per
/// approved server snapshot at startup.
#[derive(Debug, Clone)]
pub struct ServerLaunch {
    pub server_id: String,
    pub startup_timeout: Duration,
    pub call_timeout: Duration,
    /// Snapshot of `server_config_digest` at registration time. Surfaces in
    /// telemetry; also part of the pool isolation key.
    pub server_config_digest: String,
    /// Snapshot's MCP protocol version. Surfaces in telemetry only.
    pub protocol_version: String,
    /// Identity digest recorded at import time, derived from server-advertised
    /// `capabilities` + `serverInfo.name`. The runtime recomputes the same
    /// digest from the live `initialize` response and refuses to bind a
    /// connection on mismatch.
    pub expected_identity_digest: String,
    /// Server-wide tool-set digest recorded at import time. Startup verifies
    /// it with a live `tools/list` before accepting the connection; the drift
    /// handler recomputes it again on `notifications/tools/list_changed`.
    /// Any mismatch marks the snapshot stale and fails closed.
    pub expected_tools_digest: String,
    /// Cache root the resolver loaded this snapshot from. Startup verification
    /// and the drift handler use it to flip the on-disk server record to `Stale`
    /// so the runner refuses to bind this server on next startup.
    pub cache_root: PathBuf,
    /// Transport-specific launch parameters. Typestate: each variant carries
    /// exactly the fields its transport needs, so a stdio launch can never
    /// carry an HTTP URL and vice versa.
    pub kind: LaunchKind,
}

/// Per-transport launch data. Stdio carries the child-process command shape;
/// HTTP carries the pre-resolved URL, header/secret material, and network
/// policy.
#[derive(Debug, Clone)]
pub enum LaunchKind {
    Stdio(StdioLaunch),
    Http(HttpLaunchConfig),
}

#[derive(Debug, Clone)]
pub struct StdioLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Pre-resolved Streamable HTTP launch inputs. The resolver freezes the
/// resolved secret set (and fails closed on missing values) before the
/// runtime ever instantiates a transport.
#[derive(Debug, Clone)]
pub struct HttpLaunchConfig {
    pub url: String,
    pub static_headers: Vec<ConfigHttpHeader>,
    pub resolved_secrets: Vec<ResolvedSecret>,
    pub network_policy: HttpNetworkPolicyConfig,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub idle_stream_timeout: Duration,
    pub max_idle_per_host: u64,
    pub max_concurrent_requests_per_pool_key: u64,
    /// Additional CA certificates (PEM-encoded) to trust beyond the system /
    /// webpki defaults. Empty in normal operator config — the field exists so
    /// deployments with private/internal CAs (and the in-process TLS test
    /// harness) can extend the trust store without touching
    /// `danger_accept_invalid_certs`, which remains hard-off.
    pub extra_ca_certs_pem: Vec<Vec<u8>>,
}

/// Runtime client handler. Listens for `tools/list_changed` notifications
/// and flips a shared `drifted` flag; hard-denies the small set of
/// server→client capabilities we never want a tool to exercise.
#[derive(Clone)]
struct RuntimeClientHandler {
    server_id: Arc<str>,
    drifted: Arc<AtomicBool>,
    expected_tools_digest: Arc<str>,
    cache_root: Arc<PathBuf>,
    transport: &'static str,
}

impl ClientHandler for RuntimeClientHandler {
    fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<CreateMessageResult, McpError>> + MaybeSendFuture + '_ {
        std::future::ready(Err(
            McpError::method_not_found::<CreateMessageRequestMethod>(),
        ))
    }

    fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<ListRootsResult, McpError>> + MaybeSendFuture + '_ {
        std::future::ready(Err(McpError::method_not_found::<ListRootsRequestMethod>()))
    }

    fn create_elicitation(
        &self,
        _request: CreateElicitationRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<CreateElicitationResult, McpError>> + MaybeSendFuture + '_
    {
        // Hard-deny elicitation instead of silently declining.
        std::future::ready(Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "elicitation/create",
            None,
        )))
    }

    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let server_id = self.server_id.clone();
        async move {
            let message_len = params.message.as_ref().map(String::len).unwrap_or(0);
            tracing::info!(
                target: "mcp.progress",
                mcp_server_id = %server_id,
                mcp_progress_token = ?params.progress_token,
                mcp_progress = params.progress,
                mcp_progress_total = ?params.total,
                mcp_progress_has_message = params.message.is_some(),
                mcp_progress_message_len = message_len,
                event = "mcp.progress",
                "MCP server reported tool-call progress",
            );
        }
    }

    fn on_tool_list_changed(
        &self,
        context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let server_id = self.server_id.clone();
        let drifted = self.drifted.clone();
        let expected = self.expected_tools_digest.clone();
        let cache_root = self.cache_root.clone();
        let transport = self.transport;
        async move {
            // Out-of-band tools/list using the peer attached to this
            // notification. Failure here is itself a fail-closed signal:
            // we cannot prove the tool set is unchanged, so treat as drift.
            let observed = match context.peer.list_all_tools().await {
                Ok(tools) => tools,
                Err(err) => {
                    tracing::warn!(
                        target: "mcp.drift",
                        mcp_server_id = %server_id,
                        error = %err,
                        event = "mcp.tools_list_failed",
                        "out-of-band tools/list failed after list_changed; marking connection stale",
                    );
                    mark_drifted_and_persist(&server_id, &drifted, &cache_root);
                    return;
                }
            };

            let observed_digest = digest_from_live_tools(&observed);
            if observed_digest.as_str() == expected.as_ref() {
                tracing::debug!(
                    target: "mcp.drift",
                    mcp_server_id = %server_id,
                    event = "mcp.tools_list_changed_spurious",
                    "tools/list_changed received but tool set digest unchanged; ignoring",
                );
                return;
            }

            record_mcp_digest_mismatch("tools_changed", transport);
            tracing::warn!(
                target: "mcp.drift",
                mcp_server_id = %server_id,
                expected = %expected.as_ref(),
                observed = %observed_digest,
                event = "mcp.tools_list_changed",
                "MCP server tool set digest changed; marking snapshot stale, operator must re-import and approve a new registry snapshot",
            );
            mark_drifted_and_persist(&server_id, &drifted, &cache_root);
        }
    }
}

/// Compute the same `(mcp_tool_name, input_schema_digest)` digest the
/// importer wrote into the snapshot. Each schema is canonicalized through
/// the shared normalizer so on-wire formatting differences (key order,
/// whitespace) do not produce false drift signals.
fn digest_from_live_tools(tools: &[rmcp::model::Tool]) -> baml_rt_tools::mcp_snapshot::Digest {
    let normalized: Vec<(String, baml_rt_tools::mcp_snapshot::Digest)> = tools
        .iter()
        .map(|tool| {
            let schema =
                serde_json::to_value(tool.input_schema.as_ref()).unwrap_or(serde_json::Value::Null);
            (tool.name.to_string(), digest_input_schema(&schema))
        })
        .collect();
    compute_tools_digest_from_entries(
        normalized
            .iter()
            .map(|(name, digest)| (name.as_str(), digest.as_str())),
    )
}

/// Flip the in-memory drift flag and best-effort persist `Stale` to disk so
/// the runner refuses to bind this server on next startup. Disk failures
/// are logged but do not block the runtime signal.
fn mark_drifted_and_persist(server_id: &str, drifted: &AtomicBool, cache_root: &Path) {
    let already = drifted.swap(true, Ordering::SeqCst);
    if already {
        return;
    }
    match mark_server_stale(cache_root, server_id) {
        Ok(_prev) => {}
        Err(err) => {
            tracing::warn!(
                target: "mcp.drift",
                mcp_server_id = %server_id,
                error = %err,
                event = "mcp.stale_persist_failed",
                "failed to mark MCP server stale on disk; in-memory drift flag is set",
            );
        }
    }
}

/// Shared, lazily-initialized rmcp client. Cloning the connection (via `Arc`)
/// keeps every handler against the same server bound to the same process.
#[derive(Clone)]
pub struct McpCancelHandle {
    peer: Peer<RoleClient>,
    request_id: RequestId,
    local_token: CancellationToken,
}

impl McpCancelHandle {
    fn new(peer: Peer<RoleClient>, request_id: RequestId, local_token: CancellationToken) -> Self {
        Self {
            peer,
            request_id,
            local_token,
        }
    }

    pub fn cancel_local(&self) {
        self.local_token.cancel();
    }

    pub async fn notify_cancelled(&self, reason: Option<String>) -> Result<(), ServiceError> {
        let notification = CancelledNotification::new(CancelledNotificationParam {
            request_id: self.request_id.clone(),
            reason,
        });
        self.peer.send_notification(notification.into()).await
    }

    pub async fn cancel(&self, reason: Option<String>) -> Result<(), ServiceError> {
        self.cancel_local();
        match tokio::time::timeout(ABORT_CANCEL_NOTIFY_TIMEOUT, self.notify_cancelled(reason)).await
        {
            Ok(result) => result,
            Err(_) => Err(ServiceError::Timeout {
                timeout: ABORT_CANCEL_NOTIFY_TIMEOUT,
            }),
        }
    }
}

pub type McpCancelSlot = Arc<Mutex<Option<McpCancelHandle>>>;

/// Shared, lazily-initialized rmcp client. Cloning the connection (via `Arc`)
/// keeps every handler against the same server bound to the same process.
pub struct McpConnection {
    launch: ServerLaunch,
    drifted: Arc<AtomicBool>,
    service: OnceCell<Arc<RunningService<RoleClient, RuntimeClientHandler>>>,
}

impl McpConnection {
    pub fn new(launch: ServerLaunch) -> Self {
        Self {
            launch,
            drifted: Arc::new(AtomicBool::new(false)),
            service: OnceCell::new(),
        }
    }

    pub fn server_id(&self) -> &str {
        &self.launch.server_id
    }

    pub fn protocol_version(&self) -> &str {
        &self.launch.protocol_version
    }

    pub fn server_config_digest(&self) -> &str {
        &self.launch.server_config_digest
    }

    /// Returns true once startup verification or `notifications/tools/list_changed`
    /// observes drift for this connection. Fail-closed signal consumed by the
    /// handler before every `tools/call`.
    pub fn is_drifted(&self) -> bool {
        self.drifted.load(Ordering::SeqCst)
    }

    pub fn is_dead(&self) -> bool {
        self.service
            .get()
            .is_some_and(|service| service.is_closed())
    }

    pub async fn call_tool(
        &self,
        mcp_tool_name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, ConnectionError> {
        self.call_tool_with_cancel_slot(mcp_tool_name, arguments, None)
            .await
    }

    pub async fn call_tool_with_cancel_slot(
        &self,
        mcp_tool_name: &str,
        arguments: Value,
        cancel_slot: Option<McpCancelSlot>,
    ) -> Result<CallToolResult, ConnectionError> {
        if self.is_drifted() {
            return Err(ConnectionError::Stale {
                server_id: self.launch.server_id.clone(),
            });
        }
        if self.is_dead() {
            record_mcp_session_expired(self.transport_label());
            return Err(ConnectionError::SessionExpired {
                server_id: self.launch.server_id.clone(),
            });
        }
        let service = self.service().await?;
        let arguments_map = arguments_to_map(arguments)?;
        let params = match arguments_map {
            Some(map) => CallToolRequestParams::new(mcp_tool_name.to_string()).with_arguments(map),
            None => CallToolRequestParams::new(mcp_tool_name.to_string()),
        };
        let timeout = if self.launch.call_timeout.is_zero() {
            RUNTIME_CALL_TIMEOUT_DEFAULT
        } else {
            self.launch.call_timeout
        };
        let handle = service
            .send_cancellable_request(
                ClientRequest::CallToolRequest(CallToolRequest::new(params)),
                request_options_with_timeout(timeout),
            )
            .await?;
        let local_token = CancellationToken::new();
        let cancel_handle =
            McpCancelHandle::new(handle.peer.clone(), handle.id.clone(), local_token.clone());
        if let Some(slot) = &cancel_slot {
            *slot.lock().await = Some(cancel_handle.clone());
        }
        let progress_token = handle.progress_token.clone();
        tracing::debug!(
            target: "mcp.progress",
            mcp_server_id = %self.launch.server_id,
            mcp_tool_name,
            mcp_progress_token = ?progress_token,
            event = "mcp.progress_token_created",
            "MCP call progress token created",
        );
        let response = tokio::select! {
            response = handle.await_response() => response,
            () = local_token.cancelled() => {
                if let Some(slot) = &cancel_slot {
                    let _ = slot.lock().await.take();
                }
                return Err(ConnectionError::CallCancelled {
                    server_id: self.launch.server_id.clone(),
                    reason: "local abort requested".to_string(),
                });
            }
        };
        if let Some(slot) = &cancel_slot {
            let _ = slot.lock().await.take();
        }
        match response {
            Ok(ServerResult::CallToolResult(result)) => Ok(result),
            Ok(_) => Err(ConnectionError::CallTool(ServiceError::UnexpectedResponse)),
            Err(err) if service_error_is_session_expired(&err) => {
                service.cancellation_token().cancel();
                record_mcp_session_expired(self.transport_label());
                Err(ConnectionError::SessionExpired {
                    server_id: self.launch.server_id.clone(),
                })
            }
            Err(ServiceError::Timeout { .. }) => Err(ConnectionError::CallTimeout(timeout)),
            Err(err) => Err(err.into()),
        }
    }

    pub fn transport_label(&self) -> &'static str {
        transport_label(&self.launch.kind)
    }

    async fn service(
        &self,
    ) -> Result<Arc<RunningService<RoleClient, RuntimeClientHandler>>, ConnectionError> {
        self.service
            .get_or_try_init(|| async { self.spawn_service().await.map(Arc::new) })
            .await
            .cloned()
    }

    async fn spawn_service(
        &self,
    ) -> Result<RunningService<RoleClient, RuntimeClientHandler>, ConnectionError> {
        let launch = self.launch.clone();
        let drifted = self.drifted.clone();
        let timeout = if launch.startup_timeout.is_zero() {
            STARTUP_TIMEOUT_DEFAULT
        } else {
            launch.startup_timeout
        };
        let handler = RuntimeClientHandler {
            server_id: Arc::<str>::from(launch.server_id.clone()),
            drifted: drifted.clone(),
            expected_tools_digest: Arc::<str>::from(launch.expected_tools_digest.clone()),
            cache_root: Arc::new(launch.cache_root.clone()),
            transport: transport_label(&launch.kind),
        };
        let serve = async move {
            let service = match &launch.kind {
                LaunchKind::Stdio(stdio) => {
                    let command = tokio::process::Command::new(&stdio.command).configure(|cmd| {
                        cmd.args(&stdio.args).env_clear().envs(&stdio.env);
                    });
                    let (transport, stderr) = TokioChildProcess::builder(command)
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .map_err(|err| ConnectionError::Spawn {
                            command: stdio.command.clone(),
                            source: err,
                        })?;
                    spawn_stderr_drain(launch.server_id.clone(), stderr);
                    handler
                        .serve(transport)
                        .await
                        .map_err(|err| ConnectionError::InitializeFailed(err.to_string()))?
                }
                LaunchKind::Http(http) => {
                    let transport = build_rmcp_http_transport(&launch.server_id, http).map_err(
                        |err| match err {
                            HttpTransportBuildError::Policy(p) => {
                                ConnectionError::Transport(p.to_string())
                            }
                            HttpTransportBuildError::Header(h) => {
                                ConnectionError::Transport(h.to_string())
                            }
                            HttpTransportBuildError::InvalidAuthHeader { name, reason } => {
                                ConnectionError::Transport(format!(
                                    "invalid auth header `{name}`: {reason}"
                                ))
                            }
                            HttpTransportBuildError::InvalidExtraCaCert { index, reason } => {
                                ConnectionError::Transport(format!(
                                    "invalid extra CA certificate (PEM #{index}): {reason}"
                                ))
                            }
                            HttpTransportBuildError::Client(err) => ConnectionError::Transport(
                                format!("reqwest client build failed: {err}"),
                            ),
                        },
                    )?;
                    handler
                        .serve(transport)
                        .await
                        .map_err(|err| ConnectionError::InitializeFailed(err.to_string()))?
                }
            };
            verify_server_identity(&service, &launch).await?;
            verify_startup_tools_digest(&service, &launch, &drifted).await?;
            Ok(service)
        };
        match tokio::time::timeout(timeout, serve).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionError::InitializeTimeout(timeout)),
        }
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        // If the rmcp service is initialised, signal cancel so the child
        // process is reaped. Two failure modes to handle explicitly:
        //   1. Drop runs on a thread with no tokio runtime — `tokio::spawn`
        //      would panic. Detect via `Handle::try_current()` and fall back
        //      to firing the cancellation token (terminates the read loop;
        //      `TokioChildProcess::Drop` then reaps via `kill_on_drop`).
        //   2. The `Arc<RunningService>` still has live clones (e.g. an
        //      in-flight call). `Arc::try_unwrap` then fails silently; log a
        //      warning so leaked children are at least observable.
        let Some(service_arc) = self.service.take() else {
            return;
        };
        match Arc::try_unwrap(service_arc) {
            Ok(service) => match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = service.cancel().await;
                    });
                }
                Err(_) => {
                    // No runtime present: cancel the token synchronously and
                    // let `Drop` on `RunningService` / its transport reap.
                    service.cancellation_token().cancel();
                }
            },
            Err(still_shared) => {
                tracing::warn!(
                    target: "mcp.runtime",
                    server_id = %self.launch.server_id,
                    strong_count = Arc::strong_count(&still_shared),
                    event = "mcp.connection_drop_leaks_service",
                    "McpConnection dropped while rmcp service still has outstanding clones; \
                     child process will not be reaped until the last clone is released",
                );
                // Best-effort: fire cancellation through the shared handle so
                // any future awaiting the read loop unblocks.
                still_shared.cancellation_token().cancel();
            }
        }
    }
}

/// Reads the live `initialize` result from the established rmcp session,
/// recomputes the identity digest, and compares it to the value frozen in
/// the approved snapshot. Mismatch cancels the freshly-built service so the
/// child process is reaped before the error bubbles up.
async fn verify_server_identity(
    service: &RunningService<RoleClient, RuntimeClientHandler>,
    launch: &ServerLaunch,
) -> Result<(), ConnectionError> {
    let Some(peer_info) = service.peer().peer_info() else {
        signal_cancel(service).await;
        return Err(ConnectionError::MissingPeerInfo {
            server_id: launch.server_id.clone(),
        });
    };
    let capabilities = match to_value(&peer_info.capabilities) {
        Ok(value) => value,
        Err(err) => {
            signal_cancel(service).await;
            return Err(ConnectionError::IdentitySerializeFailed {
                server_id: launch.server_id.clone(),
                source: err,
            });
        }
    };
    let server_info = match to_value(&peer_info.server_info) {
        Ok(value) => value,
        Err(err) => {
            signal_cancel(service).await;
            return Err(ConnectionError::IdentitySerializeFailed {
                server_id: launch.server_id.clone(),
                source: err,
            });
        }
    };
    let observed = compute_server_identity_digest(&capabilities, &server_info);
    if observed.as_str() != launch.expected_identity_digest {
        record_mcp_digest_mismatch("identity", transport_label(&launch.kind));
        tracing::warn!(
            target: "mcp.identity",
            mcp_server_id = %launch.server_id,
            expected = %launch.expected_identity_digest,
            observed = %observed,
            event = "mcp.identity_mismatch",
            "MCP server identity digest does not match approved snapshot; refusing to bind connection",
        );
        signal_cancel(service).await;
        return Err(ConnectionError::IdentityMismatch {
            server_id: launch.server_id.clone(),
            expected: launch.expected_identity_digest.clone(),
            observed: observed.as_str().to_string(),
        });
    }
    Ok(())
}

/// Verify live startup tool surface before accepting the connection. This
/// closes the gap where a restarted server changes schemas but never emits
/// `notifications/tools/list_changed`.
async fn verify_startup_tools_digest(
    service: &RunningService<RoleClient, RuntimeClientHandler>,
    launch: &ServerLaunch,
    drifted: &AtomicBool,
) -> Result<(), ConnectionError> {
    let observed_tools = match service.peer().list_all_tools().await {
        Ok(tools) => tools,
        Err(err) => {
            signal_cancel(service).await;
            return Err(ConnectionError::StartupToolsListFailed {
                server_id: launch.server_id.clone(),
                reason: err.to_string(),
            });
        }
    };
    let observed = digest_from_live_tools(&observed_tools);
    if observed.as_str() != launch.expected_tools_digest {
        record_mcp_digest_mismatch("tools_startup", transport_label(&launch.kind));
        tracing::warn!(
            target: "mcp.drift",
            mcp_server_id = %launch.server_id,
            expected = %launch.expected_tools_digest,
            observed = %observed,
            event = "mcp.startup_tools_digest_mismatch",
            "MCP server tool surface digest does not match approved snapshot at startup; refusing to bind connection",
        );
        mark_drifted_and_persist(&launch.server_id, drifted, &launch.cache_root);
        signal_cancel(service).await;
        return Err(ConnectionError::ToolsDigestMismatch {
            server_id: launch.server_id.clone(),
            expected: launch.expected_tools_digest.clone(),
            observed: observed.as_str().to_string(),
        });
    }
    Ok(())
}

fn spawn_stderr_drain(server_id: String, stderr: Option<tokio::process::ChildStderr>) {
    const MAX_CAPTURED_STDERR: usize = 64 * 1024;

    let Some(mut stderr) = stderr else {
        return;
    };
    tokio::spawn(async move {
        let mut captured = Vec::new();
        let mut total = 0usize;
        let mut chunk = [0u8; 4096];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    total = total.saturating_add(n);
                    let remaining = MAX_CAPTURED_STDERR.saturating_sub(captured.len());
                    if remaining > 0 {
                        captured.extend_from_slice(&chunk[..n.min(remaining)]);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        target: "mcp.stdio",
                        mcp_server_id = %server_id,
                        error = %err,
                        "failed to drain MCP stdio server stderr"
                    );
                    return;
                }
            }
        }
        if captured.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&captured);
        tracing::debug!(
            target: "mcp.stdio",
            mcp_server_id = %server_id,
            stderr = %text,
            stderr_bytes = total,
            stderr_truncated = total > captured.len(),
            "MCP stdio server stderr"
        );
    });
}

/// Signal the rmcp service to wind down without consuming it.
///
/// `rmcp::RunningService::cancel` takes `self`, but our callers hold a
/// borrow because the service is owned by the surrounding `OnceCell` /
/// `Arc`. Firing the cancellation token has the same effect on the child
/// process — it terminates the read loop, which lets `RunningService`'s
/// own `Drop` close the transport, and `TokioChildProcess::Drop` then reaps
/// the child via `kill_on_drop` (Linux additionally has
/// `PR_SET_PDEATHSIG=SIGKILL` from the sandbox layer as a safety net).
async fn signal_cancel(service: &RunningService<RoleClient, RuntimeClientHandler>) {
    service.cancellation_token().cancel();
}

fn transport_label(kind: &LaunchKind) -> &'static str {
    match kind {
        LaunchKind::Stdio(_) => "stdio",
        LaunchKind::Http(_) => "streamable_http",
    }
}

fn request_options_with_timeout(timeout: Duration) -> PeerRequestOptions {
    let mut options = PeerRequestOptions::no_options();
    options.timeout = Some(timeout);
    options
}

fn service_error_is_session_expired(err: &ServiceError) -> bool {
    let ServiceError::TransportSend(transport_err) = err else {
        return false;
    };

    // Streamable HTTP session expiry is currently carried as
    // `StreamableHttpError<reqwest::Error>` inside TransportSend. Future rmcp
    // transports may box different error types; non-matching downcasts are not
    // session expiry and must fall through to the existing CallTool mapping.
    let mut source: Option<&(dyn StdError + 'static)> = Some(transport_err.error.as_ref());
    while let Some(err) = source {
        if let Some(http_err) = err.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            return matches!(http_err, StreamableHttpError::SessionExpired);
        }
        source = err.source();
    }
    false
}

fn arguments_to_map(
    arguments: Value,
) -> Result<Option<serde_json::Map<String, Value>>, ConnectionError> {
    match arguments {
        Value::Null => Ok(None),
        Value::Object(map) => Ok(Some(map)),
        other => Err(ConnectionError::InvalidArguments(
            value_kind(&other).to_string(),
        )),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
