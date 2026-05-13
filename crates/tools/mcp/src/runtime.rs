//! Long-lived MCP client connection used by the runtime tool handler.
//!
//! Each `McpConnection` owns a single server-process subscription. The first
//! `call_tool` on the connection spawns the child, establishes the rmcp
//! `RunningService`, and caches it; subsequent calls reuse it. The connection
//! is shared across every tool whose snapshot resolves to the same server,
//! so an agent that uses multiple Grafana MCP tools sees one child process.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::{RoleClient, RunningService, ServiceError},
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::sandbox::SpawnSpec;

/// Maximum time we wait for `initialize + tools/list` during lazy connection
/// startup. Caps a misbehaving server's hold on the calling task.
const STARTUP_TIMEOUT_DEFAULT: Duration = Duration::from_secs(30);

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

/// Shared, lazily-initialized rmcp client. Cloning the connection (via `Arc`)
/// keeps every handler against the same server bound to the same process.
pub struct McpConnection {
    launch: ServerLaunch,
    service: OnceCell<Arc<RunningService<RoleClient, ()>>>,
}

impl McpConnection {
    pub fn new(launch: ServerLaunch) -> Self {
        Self {
            launch,
            service: OnceCell::new(),
        }
    }

    pub fn server_id(&self) -> &str {
        &self.launch.server_id
    }

    pub async fn call_tool(
        &self,
        mcp_tool_name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, ConnectionError> {
        let service = self.service().await?;
        let arguments_map = arguments_to_map(arguments)?;
        let params = match arguments_map {
            Some(map) => CallToolRequestParams::new(mcp_tool_name.to_string()).with_arguments(map),
            None => CallToolRequestParams::new(mcp_tool_name.to_string()),
        };
        Ok(service.call_tool(params).await?)
    }

    async fn service(&self) -> Result<Arc<RunningService<RoleClient, ()>>, ConnectionError> {
        self.service
            .get_or_try_init(|| async { self.spawn_service().await.map(Arc::new) })
            .await
            .cloned()
    }

    async fn spawn_service(&self) -> Result<RunningService<RoleClient, ()>, ConnectionError> {
        let launch = self.launch.clone();
        let timeout = if launch.startup_timeout.is_zero() {
            STARTUP_TIMEOUT_DEFAULT
        } else {
            launch.startup_timeout
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
            ().serve(transport)
                .await
                .map_err(|err| ConnectionError::InitializeFailed(err.to_string()))
        };
        match tokio::time::timeout(timeout, serve).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionError::InitializeTimeout(timeout)),
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
