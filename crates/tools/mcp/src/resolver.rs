//! `ExternalToolResolver` implementations for MCP-imported tools.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_builder_catalog::project_tool,
    mcp_cache::{ToolRecord, read_server, read_tool},
    mcp_config::McpServersFile,
    tools::{ToolFunctionMetadata, ToolHandler, ToolName},
};

use crate::{
    handler::McpToolHandler,
    importer::SecretResolver,
    runtime::{McpConnection, ServerLaunch},
};

/// Default per-server startup deadline when not specified in `mcp-servers.json`.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 30;

/// Resolver that loads MCP-imported tools from a snapshot cache and binds
/// each one to an `McpConnection` whose launch parameters come from an
/// operator-supplied `mcp-servers.json`.
///
/// Connections are pooled per `server_id`, so multiple tools from the same
/// server share one MCP child process.
pub struct McpResolver<R: SecretResolver + Send + Sync> {
    cache_root: PathBuf,
    servers: McpServersFile,
    secret_resolver: R,
    connections: Mutex<HashMap<String, Arc<McpConnection>>>,
}

impl<R: SecretResolver + Send + Sync> McpResolver<R> {
    pub fn new(cache_root: PathBuf, servers: McpServersFile, secret_resolver: R) -> Self {
        Self {
            cache_root,
            servers,
            secret_resolver,
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Look up or initialise the connection for a given server id. Returns
    /// `None` when the server is unknown to the operator's `mcp-servers.json`.
    fn connection_for(&self, server_id: &str) -> Result<Option<Arc<McpConnection>>> {
        let Some(server_config) = self.servers.servers.get(server_id) else {
            return Ok(None);
        };
        let mut env = server_config.env.clone();
        if let Ok(path) = std::env::var("PATH") {
            env.entry("PATH".into()).or_insert(path);
        }
        for secret in &server_config.secrets {
            let value = self.secret_resolver.resolve(&secret.name).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "MCP server `{server_id}` requires secret `{}`, but no value was resolved",
                    secret.name
                ))
            })?;
            env.insert(secret.name.clone(), value);
        }
        let startup_timeout_secs = server_config
            .sandbox
            .as_ref()
            .and_then(|s| s.import_timeout_secs)
            .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS);

        let launch = ServerLaunch {
            server_id: server_id.to_string(),
            command: server_config.command.clone(),
            args: server_config.args.clone(),
            env,
            startup_timeout: Duration::from_secs(startup_timeout_secs),
        };

        let mut guard = self.connections.lock().unwrap();
        let conn = guard
            .entry(server_id.to_string())
            .or_insert_with(|| Arc::new(McpConnection::new(launch)))
            .clone();
        Ok(Some(conn))
    }
}

impl<R: SecretResolver + Send + Sync> ExternalToolResolver for McpResolver<R> {
    fn resolve(
        &self,
        name: &ToolName,
    ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>> {
        let display = name.to_string();
        if !display.starts_with("mcp/") {
            return Ok(None);
        }
        let record = match read_tool(&self.cache_root, &display) {
            Ok(record) => record,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(BamlRtError::InvalidArgumentWithSource {
                    message: format!("failed to load MCP cache entry for {display}"),
                    source: Box::new(err),
                });
            }
        };
        if !record.tool.approval.state.is_approved() {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP tool {display} exists in cache but is not approved (state={:?})",
                record.tool.approval.state
            )));
        }
        let server = read_server(&self.cache_root, &record.server_id).map_err(|err| {
            BamlRtError::InvalidArgumentWithSource {
                message: format!(
                    "MCP tool {display} references server `{}` whose record is missing",
                    record.server_id
                ),
                source: Box::new(err),
            }
        })?;
        if !server.approval.state.is_approved() {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP server `{}` is not approved (state={:?}); refusing to bind handler for {display}",
                record.server_id, server.approval.state
            )));
        }
        let mcp_tool_name = record.tool.mcp_tool_name.clone();
        let metadata = project_record(&record)?;
        let Some(connection) = self.connection_for(&record.server_id)? else {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP server `{}` is approved in cache but not declared in mcp-servers.json",
                record.server_id
            )));
        };
        Ok(Some((
            metadata.clone(),
            Arc::new(McpToolHandler::new(metadata, connection, mcp_tool_name))
                as Arc<dyn ToolHandler>,
        )))
    }
}

fn project_record(record: &ToolRecord) -> Result<ToolFunctionMetadata> {
    project_tool(&record.server_id, record.clone())
}

/// Convenience: build an `McpResolver` from the default cache + config paths.
pub fn default_mcp_resolver<R: SecretResolver + Send + Sync>(
    cache_root: Option<&Path>,
    config_path: Option<&Path>,
    secret_resolver: R,
) -> Result<Option<McpResolver<R>>> {
    let cache_root = match cache_root {
        Some(path) => path.to_path_buf(),
        None => match baml_rt_tools::mcp_cache::default_cache_root() {
            Some(path) => path,
            None => return Ok(None),
        },
    };
    let config_path: PathBuf = match config_path {
        Some(path) => path.to_path_buf(),
        None => {
            let Some(home) = std::env::var_os("HOME") else {
                return Ok(None);
            };
            PathBuf::from(home)
                .join(".agent-platform")
                .join("mcp-servers.json")
        }
    };
    let raw = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to read {}", config_path.display()),
                source: Box::new(err),
            });
        }
    };
    let parsed =
        McpServersFile::parse(&raw).map_err(|err| BamlRtError::InvalidArgumentWithSource {
            message: format!("parsing {}", config_path.display()),
            source: Box::new(err),
        })?;
    Ok(Some(McpResolver::new(cache_root, parsed, secret_resolver)))
}
