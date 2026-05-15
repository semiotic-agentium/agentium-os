//! `ExternalToolResolver` implementations for MCP-imported tools.

use std::{
    collections::{BTreeMap, HashMap},
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
    mcp_snapshot::McpTransportRef,
    tools::{ToolFunctionMetadata, ToolHandler, ToolName},
};
use sha2::{Digest as _, Sha256};

use crate::{
    handler::McpToolHandler,
    importer::SecretResolver,
    runtime::{McpConnection, ServerLaunch},
};

/// Default per-server startup deadline when not specified in `mcp-servers.json`.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 30;
/// Default per-call deadline when not specified.
const DEFAULT_CALL_TIMEOUT_SECS: u64 = 120;

/// Pool isolation key. Two agents that resolve the same `server_id` against
/// different operator configs or different secret identities get separate
/// child processes, preserving secret isolation across tenants.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    server_id: String,
    server_config_digest: String,
    secret_identity_hash: String,
}

/// Resolver that loads MCP-imported tools from a snapshot cache and binds
/// each one to an `McpConnection` whose launch parameters come from an
/// operator-supplied `mcp-servers.json`.
///
/// Connections are pooled per (server_id, server_config_digest,
/// secret_identity_hash) — multiple tools from the same server share one MCP
/// child process iff they were registered with the same operator config and
/// same resolved secret values.
pub struct McpResolver<R: SecretResolver + Send + Sync> {
    cache_root: PathBuf,
    servers: McpServersFile,
    secret_resolver: R,
    connections: Mutex<HashMap<PoolKey, Arc<McpConnection>>>,
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

    /// Look up or initialise the connection for a given server id.
    ///
    /// `server_config_digest` is the digest cached at import time; the
    /// resolver folds it into the pool key so a config rewrite at runtime
    /// can never silently share a stale connection.
    fn connection_for(
        &self,
        server_id: &str,
        server_config_digest: &str,
        protocol_version: &str,
        expected_identity_digest: &str,
        expected_tools_digest: &str,
    ) -> Result<Option<Arc<McpConnection>>> {
        let Some(server_config) = self.servers.servers.get(server_id) else {
            return Ok(None);
        };
        let mut env = server_config.env.clone();
        if let Ok(path) = std::env::var("PATH") {
            env.entry("PATH".into()).or_insert(path);
        }
        // Resolved secrets — kept in sorted order so the identity hash is
        // stable.
        let mut resolved_secrets: BTreeMap<String, String> = BTreeMap::new();
        for secret in &server_config.secrets {
            let value = self.secret_resolver.resolve(&secret.name).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "MCP server `{server_id}` requires secret `{}`, but no value was resolved",
                    secret.name
                ))
            })?;
            resolved_secrets.insert(secret.name.clone(), value);
        }
        for (name, value) in &resolved_secrets {
            env.insert(name.clone(), value.clone());
        }
        let startup_timeout_secs = server_config
            .sandbox
            .as_ref()
            .and_then(|s| s.import_timeout_secs)
            .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS);
        let call_timeout_secs = server_config
            .sandbox
            .as_ref()
            .and_then(|s| s.runtime_call_timeout_secs)
            .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS);
        let secret_identity_hash = hash_secret_identity(&resolved_secrets);

        let launch = ServerLaunch {
            server_id: server_id.to_string(),
            command: server_config.command.clone(),
            args: server_config.args.clone(),
            env,
            startup_timeout: Duration::from_secs(startup_timeout_secs),
            call_timeout: Duration::from_secs(call_timeout_secs),
            server_config_digest: server_config_digest.to_string(),
            protocol_version: protocol_version.to_string(),
            expected_identity_digest: expected_identity_digest.to_string(),
            expected_tools_digest: expected_tools_digest.to_string(),
            cache_root: self.cache_root.clone(),
        };

        let key = PoolKey {
            server_id: server_id.to_string(),
            server_config_digest: server_config_digest.to_string(),
            secret_identity_hash,
        };
        // Recover from a poisoned mutex rather than propagating panic: a
        // single panic inside `or_insert_with` would otherwise turn every
        // future MCP resolve into a panic for the lifetime of the process.
        // The pool's invariant is "every entry is a live `Arc<McpConnection>`",
        // which is preserved across poison because we only ever insert.
        let mut guard = match self.connections.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!(
                    target: "mcp.resolver",
                    event = "mcp.pool_mutex_poisoned",
                    mcp_server_id = %server_id,
                    "MCP connection pool mutex was poisoned by a prior panic; recovering inner state",
                );
                poisoned.into_inner()
            }
        };
        let conn = guard
            .entry(key)
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
        // PR5 hardening: HTTP transport is reserved but not yet bound in the
        // runtime. Reject explicitly so a snapshot we can parse but not run
        // does not silently fall through to "missing server config".
        if matches!(server.transport, McpTransportRef::Http { .. }) {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP tool {display} uses HTTP transport which is not enabled in this build"
            )));
        }
        let mcp_tool_name = record.tool.mcp_tool_name.clone();
        let metadata = project_record(&record)?;
        let Some(connection) = self.connection_for(
            &record.server_id,
            server.server_config_digest.as_str(),
            &server.protocol_version,
            server.server_identity_digest.as_str(),
            server.tools_digest.as_str(),
        )?
        else {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP server `{}` is approved in cache but not declared in mcp-servers.json",
                record.server_id
            )));
        };
        let schema_digest = record.tool.input_schema_digest.to_string();
        Ok(Some((
            metadata.clone(),
            Arc::new(McpToolHandler::new(
                metadata,
                connection,
                mcp_tool_name,
                schema_digest,
            )) as Arc<dyn ToolHandler>,
        )))
    }
}

fn project_record(record: &ToolRecord) -> Result<ToolFunctionMetadata> {
    project_tool(&record.server_id, record.clone())
}

/// Hash the resolved secret identity (`{name: value}` pairs in sorted order).
/// Used as part of the connection pool key so two agents with different
/// secret values never share an MCP child process. The hash itself never
/// leaks any secret content.
///
/// Each field is **length-prefixed** with its big-endian u64 byte length
/// before its bytes. A naive NUL separator allows pool-key collisions when
/// secret names or values contain `\0` — e.g. `(A, "B\0C=D")` and `(A=B,
/// "C\0D")` would otherwise hash identically and route two tenants with
/// different secrets to the same MCP child process, breaching the
/// secret-isolation boundary.
fn hash_secret_identity(secrets: &BTreeMap<String, String>) -> String {
    if secrets.is_empty() {
        return "sha256:empty".to_string();
    }
    let mut hasher = Sha256::new();
    for (name, value) in secrets {
        let name_bytes = name.as_bytes();
        let value_bytes = value.as_bytes();
        hasher.update((name_bytes.len() as u64).to_be_bytes());
        hasher.update(name_bytes);
        hasher.update((value_bytes.len() as u64).to_be_bytes());
        hasher.update(value_bytes);
    }
    format!("sha256:{:x}", hasher.finalize())
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
                .join(".agentium-os")
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
