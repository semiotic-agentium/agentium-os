//! Long-lived MCP client connection used by the runtime tool handler.
//!
//! Each `McpConnection` owns a single server-process subscription. The first
//! `call_tool` on the connection spawns the child, establishes the rmcp
//! `RunningService`, and caches it; subsequent calls reuse it. The connection
//! is shared across every tool whose snapshot resolves to the same server,
//! so an agent that uses multiple Grafana MCP tools sees one child process.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rmcp::{
    ErrorData as McpError, ServiceExt,
    handler::client::ClientHandler,
    model::{
        CallToolRequestParams, CallToolResult, CreateElicitationRequestParams,
        CreateElicitationResult, CreateMessageRequestMethod, CreateMessageRequestParams,
        CreateMessageResult, ErrorCode, ListRootsRequestMethod, ListRootsResult,
    },
    service::{
        MaybeSendFuture, NotificationContext, RequestContext, RoleClient, RunningService,
        ServiceError,
    },
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::sandbox::SpawnSpec;

/// Maximum time we wait for `initialize + tools/list` during lazy connection
/// startup. Caps a misbehaving server's hold on the calling task.
const STARTUP_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);
/// Default per-call timeout used when the operator config does not specify one.
const RUNTIME_CALL_TIMEOUT_DEFAULT: Duration = Duration::from_secs(120);

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
    #[error("MCP arguments must be a JSON object, got {0}")]
    InvalidArguments(String),
    #[error("MCP call timed out after {0:?}")]
    CallTimeout(Duration),
    #[error(
        "MCP server `{server_id}` is stale (tools/list_changed received); operator must re-run mcp-review/mcp-enable"
    )]
    Stale { server_id: String },
}

/// Spawn parameters frozen at registration time. The runtime never reads MCP
/// config from disk during a tool call; the resolver builds one of these per
/// approved server snapshot at startup.
#[derive(Debug, Clone)]
pub struct ServerLaunch {
    pub server_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub startup_timeout: Duration,
    pub call_timeout: Duration,
    /// Snapshot of `server_config_digest` at registration time. Surfaces in
    /// telemetry; also part of the pool isolation key.
    pub server_config_digest: String,
    /// Snapshot's MCP protocol version. Surfaces in telemetry only.
    pub protocol_version: String,
}

impl ServerLaunch {
    pub fn to_spawn_spec(&self) -> SpawnSpec {
        SpawnSpec {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            timeout: self.startup_timeout,
        }
    }
}

/// Runtime client handler. Listens for `tools/list_changed` notifications
/// and flips a shared `drifted` flag; hard-denies the small set of
/// server→client capabilities we never want a tool to exercise.
#[derive(Clone)]
struct RuntimeClientHandler {
    server_id: Arc<str>,
    drifted: Arc<AtomicBool>,
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
        // Hard-deny elicitation; PR4 default would silently decline.
        std::future::ready(Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "elicitation/create",
            None,
        )))
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let server_id = self.server_id.clone();
        let drifted = self.drifted.clone();
        async move {
            let already = drifted.swap(true, Ordering::SeqCst);
            if !already {
                tracing::warn!(
                    target: "mcp.drift",
                    mcp_server_id = %server_id,
                    event = "mcp.tools_list_changed",
                    "MCP server signalled tools/list_changed; marking connection stale until operator re-runs mcp-review",
                );
            }
        }
    }
}

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

    /// Returns true once any `notifications/tools/list_changed` has been
    /// observed for this connection. Fail-closed signal consumed by the
    /// handler before every `tools/call`.
    pub fn is_drifted(&self) -> bool {
        self.drifted.load(Ordering::SeqCst)
    }

    pub async fn call_tool(
        &self,
        mcp_tool_name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, ConnectionError> {
        if self.is_drifted() {
            return Err(ConnectionError::Stale {
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
        match tokio::time::timeout(timeout, service.call_tool(params)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(err.into()),
            Err(_) => Err(ConnectionError::CallTimeout(timeout)),
        }
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
            drifted,
        };
        let serve = async move {
            let command = tokio::process::Command::new(&launch.command);
            let transport = TokioChildProcess::new(command.configure(|cmd| {
                cmd.args(&launch.args)
                    .env_clear()
                    .envs(&launch.env)
                    .stderr(std::process::Stdio::piped());
            }))
            .map_err(|err| ConnectionError::Spawn {
                command: launch.command.clone(),
                source: err,
            })?;
            handler
                .serve(transport)
                .await
                .map_err(|err| ConnectionError::InitializeFailed(err.to_string()))
        };
        match tokio::time::timeout(timeout, serve).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionError::InitializeTimeout(timeout)),
        }
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        // If the rmcp service is initialised, fire-and-forget a cancel so the
        // child process is reaped instead of inheriting our tokio runtime.
        // OnceCell's sync `take` is only available with `&mut self`.
        let maybe_service = self.service.take();
        if let Some(service_arc) = maybe_service
            && let Ok(service) = Arc::try_unwrap(service_arc)
        {
            tokio::spawn(async move {
                let _ = service.cancel().await;
            });
        }
    }
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
