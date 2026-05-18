//! `ExternalToolResolver` implementations for MCP-imported tools.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use baml_rt_core::{BamlRtError, Result};
use baml_rt_observability::record_mcp_registry_entry_recreated;
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_builder_catalog::project_tool,
    mcp_cache::{ToolRecord, read_server, read_tool},
    mcp_config::{
        McpServerTransportConfig, McpServersFile, SecretInjection, SecretSource,
        StreamableHttpConfig,
    },
    mcp_secrets::{
        ResolvedSecret, SecretFingerprint, compute_secret_fingerprint, resolve_secret_specs,
    },
    mcp_snapshot::{Digest as McpDigest, compute_server_config_digest},
    tools::{ToolFunctionMetadata, ToolHandler, ToolName},
};
use dashmap::{DashMap, mapref::entry::Entry};

use crate::{
    handler::McpToolHandler,
    importer::SecretResolver,
    runtime::{HttpLaunchConfig, LaunchKind, McpConnection, ServerLaunch, StdioLaunch},
};

/// Default per-server startup deadline when not specified in `mcp-servers.json`.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 30;
/// Default per-call deadline when not specified.
const DEFAULT_CALL_TIMEOUT_SECS: u64 = 120;

/// Wire transport discriminant carried on the pool key so a registry rewrite
/// from stdio to Streamable HTTP (or vice versa) never silently reuses a
/// stale connection.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum PoolTransport {
    Stdio,
    StreamableHttp,
}

/// Pool isolation key. Two agents that resolve the same `server_id` against
/// different operator configs, different secret identities, or via a
/// different transport get separate connections.
///
/// `agent_scope` is the resolver-bound agent identity (typically the agent
/// package name). One `McpResolver` instance is bound to one agent scope at
/// construction; isolation across agents falls out of that — different
/// agents construct different resolvers, each with its own pool.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct PoolKey {
    agent_scope: Arc<str>,
    server_id: String,
    server_config_digest: String,
    secret_fingerprint: SecretFingerprint,
    transport: PoolTransport,
    protocol_version: String,
}

/// Resolver that loads MCP-imported tools from a snapshot cache and binds
/// each one to an `McpConnection` whose launch parameters come from an
/// operator-supplied `mcp-servers.json`.
///
/// One resolver per agent scope. Connections are pooled inside the resolver
/// per (server_id, server_config_digest, secret_fingerprint, transport,
/// protocol_version). Cross-agent isolation is implicit: each agent gets
/// its own resolver instance and therefore its own pool.
pub struct McpResolver<R: SecretResolver + Send + Sync> {
    agent_scope: Arc<str>,
    cache_root: PathBuf,
    servers: McpServersFile,
    secret_resolver: R,
    connections: DashMap<PoolKey, Arc<McpConnection>>,
}

impl<R: SecretResolver + Send + Sync> McpResolver<R> {
    /// Construct a resolver scoped to the global runner identity. Use this
    /// for stand-alone runners and tests where there is no per-agent
    /// distinction.
    pub fn new(cache_root: PathBuf, servers: McpServersFile, secret_resolver: R) -> Self {
        Self::for_agent("global", cache_root, servers, secret_resolver)
    }

    /// Construct a resolver bound to a specific agent scope. The scope name
    /// becomes part of every pool key produced by this resolver.
    pub fn for_agent(
        agent_scope: impl Into<String>,
        cache_root: PathBuf,
        servers: McpServersFile,
        secret_resolver: R,
    ) -> Self {
        Self {
            agent_scope: Arc::<str>::from(agent_scope.into()),
            cache_root,
            servers,
            secret_resolver,
            connections: DashMap::new(),
        }
    }

    /// Look up or initialise the connection for a given server id.
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
        // Recompute the launch-config digest from current operator config and
        // refuse to bind on mismatch. Uses the same digest inputs the importer
        // sealed at approval time: protocol version + the approved tool-set
        // digest. Any drift in command/args/env-keys/auth shape/policy here
        // means the running config no longer matches what was approved.
        let tools_digest = McpDigest::new(expected_tools_digest.to_string());
        let observed_config_digest = compute_server_config_digest(
            server_id,
            protocol_version,
            server_config,
            Some(&tools_digest),
        );
        if observed_config_digest.as_str() != server_config_digest {
            return Err(BamlRtError::InvalidArgument(format!(
                "MCP server `{server_id}` launch config digest mismatch (expected `{server_config_digest}`, observed `{observed_config_digest}`); operator must re-import and approve a new registry snapshot"
            )));
        }

        // Resolve every declared secret through the unified spec model. Fails
        // closed before any transport is constructed.
        let specs = server_config.secret_specs();
        let resolved_map = resolve_secret_specs(&specs, |source| match source {
            SecretSource::Env { name } => self.secret_resolver.resolve(name),
        })
        .map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "MCP server `{server_id}` secret resolution failed: {err}"
            ))
        })?;
        let resolved_vec: Vec<ResolvedSecret> = resolved_map.values().cloned().collect();

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

        let secret_fingerprint = compute_secret_fingerprint(&resolved_vec);

        let (kind, transport) = match &server_config.transport {
            None => {
                // Stdio path. Inject env-kind secrets into the child env.
                let mut env = server_config.env.clone();
                if let Ok(path) = std::env::var("PATH") {
                    env.entry("PATH".into()).or_insert(path);
                }
                for sec in &resolved_vec {
                    if let SecretInjection::Env { name } = &sec.spec.inject {
                        env.insert(name.clone(), sec.value.clone());
                    }
                }
                (
                    LaunchKind::Stdio(StdioLaunch {
                        command: server_config.command.clone(),
                        args: server_config.args.clone(),
                        env,
                    }),
                    PoolTransport::Stdio,
                )
            }
            Some(McpServerTransportConfig::StreamableHttp(http_cfg)) => {
                let http_launch = build_http_launch(http_cfg, resolved_vec.clone());
                (LaunchKind::Http(http_launch), PoolTransport::StreamableHttp)
            }
        };

        let launch = ServerLaunch {
            server_id: server_id.to_string(),
            startup_timeout: Duration::from_secs(startup_timeout_secs),
            call_timeout: Duration::from_secs(call_timeout_secs),
            server_config_digest: server_config_digest.to_string(),
            protocol_version: protocol_version.to_string(),
            expected_identity_digest: expected_identity_digest.to_string(),
            expected_tools_digest: expected_tools_digest.to_string(),
            cache_root: self.cache_root.clone(),
            kind,
        };

        let key = PoolKey {
            agent_scope: self.agent_scope.clone(),
            server_id: server_id.to_string(),
            server_config_digest: server_config_digest.to_string(),
            secret_fingerprint,
            transport,
            protocol_version: protocol_version.to_string(),
        };
        let conn = match self.connections.entry(key) {
            Entry::Occupied(mut entry) if entry.get().is_dead() => {
                tracing::warn!(
                    target: "mcp.registry",
                    mcp_server_id = %server_id,
                    agent_scope = %self.agent_scope,
                    event = "mcp.session_expired",
                    "MCP connection service is cancelled; evicting registry entry",
                );
                let conn = Arc::new(McpConnection::new(launch));
                entry.insert(conn.clone());
                record_mcp_registry_entry_recreated(pool_transport_label(transport));
                tracing::warn!(
                    target: "mcp.registry",
                    mcp_server_id = %server_id,
                    agent_scope = %self.agent_scope,
                    event = "mcp.registry.entry_recreated",
                    "MCP registry entry recreated after session expiry",
                );
                conn
            }
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => entry.insert(Arc::new(McpConnection::new(launch))).clone(),
        };
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

fn pool_transport_label(transport: PoolTransport) -> &'static str {
    match transport {
        PoolTransport::Stdio => "stdio",
        PoolTransport::StreamableHttp => "streamable_http",
    }
}

fn build_http_launch(
    http: &StreamableHttpConfig,
    resolved_secrets: Vec<ResolvedSecret>,
) -> HttpLaunchConfig {
    HttpLaunchConfig {
        url: http.url.clone(),
        static_headers: http.headers.clone(),
        resolved_secrets,
        network_policy: http.network_policy.clone(),
        connect_timeout: Duration::from_millis(http.timeouts.connect_ms),
        request_timeout: Duration::from_millis(http.timeouts.request_ms),
        idle_stream_timeout: Duration::from_millis(http.timeouts.idle_stream_ms),
        max_idle_per_host: http.pooling.max_idle_per_host,
        max_concurrent_requests_per_pool_key: http.pooling.max_concurrent_requests_per_pool_key,
        extra_ca_certs_pem: Vec::new(),
    }
}

/// Env-var override for the operator's `mcp-servers.json` path.
pub const MCP_SERVERS_CONFIG_ENV: &str = "BAML_MCP_SERVERS_CONFIG";

/// Default `mcp-servers.json` path when the env-var override is unset.
const MCP_SERVERS_CONFIG_DEFAULT: &str = ".agentium-os/mcp-servers.json";

/// Build an `McpResolver` from a registry-derived snapshot cache and the
/// operator's launch config (`mcp-servers.json`).
pub fn default_mcp_resolver<R: SecretResolver + Send + Sync>(
    cache_root: &Path,
    secret_resolver: R,
) -> Result<McpResolver<R>> {
    default_mcp_resolver_for_agent("global", cache_root, secret_resolver)
}

/// Same as [`default_mcp_resolver`] but bound to an explicit agent scope so
/// pool keys carry the agent identity.
pub fn default_mcp_resolver_for_agent<R: SecretResolver + Send + Sync>(
    agent_scope: impl Into<String>,
    cache_root: &Path,
    secret_resolver: R,
) -> Result<McpResolver<R>> {
    let config_path = resolve_servers_config_path()?;
    let raw = std::fs::read_to_string(&config_path).map_err(|err| {
        BamlRtError::InvalidArgumentWithSource {
            message: format!(
                "MCP servers config required but unreadable at {} \
                 (set {MCP_SERVERS_CONFIG_ENV} or place the file at the default path)",
                config_path.display()
            ),
            source: Box::new(err),
        }
    })?;
    let parsed =
        McpServersFile::parse(&raw).map_err(|err| BamlRtError::InvalidArgumentWithSource {
            message: format!("parsing {}", config_path.display()),
            source: Box::new(err),
        })?;
    Ok(McpResolver::for_agent(
        agent_scope,
        cache_root.to_path_buf(),
        parsed,
        secret_resolver,
    ))
}

fn resolve_servers_config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(MCP_SERVERS_CONFIG_ENV)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Err(BamlRtError::InvalidArgument(format!(
            "MCP servers config required but neither {MCP_SERVERS_CONFIG_ENV} \
             nor HOME is set; cannot locate mcp-servers.json"
        )));
    };
    Ok(PathBuf::from(home).join(MCP_SERVERS_CONFIG_DEFAULT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(value: &str) -> SecretFingerprint {
        compute_secret_fingerprint(&[ResolvedSecret {
            spec: baml_rt_tools::mcp_config::SecretSpec {
                id: "secret".into(),
                source: SecretSource::Env { name: "S".into() },
                inject: SecretInjection::Env { name: "S".into() },
                version: None,
            },
            value: value.into(),
        }])
    }

    fn key(agent: &str, server: &str, transport: PoolTransport, secret_value: &str) -> PoolKey {
        PoolKey {
            agent_scope: Arc::<str>::from(agent),
            server_id: server.into(),
            server_config_digest: "sha256:cfg".into(),
            secret_fingerprint: fingerprint(secret_value),
            transport,
            protocol_version: "2025-06-18".into(),
        }
    }

    #[test]
    fn pool_key_distinguishes_agents() {
        let a = key("agent-a", "grafana", PoolTransport::Stdio, "h");
        let b = key("agent-b", "grafana", PoolTransport::Stdio, "h");
        assert_ne!(a, b);
    }

    #[test]
    fn pool_key_distinguishes_transports() {
        let a = key("agent-a", "grafana", PoolTransport::Stdio, "h");
        let b = key("agent-a", "grafana", PoolTransport::StreamableHttp, "h");
        assert_ne!(a, b);
    }

    #[test]
    fn pool_key_distinguishes_secret_identity() {
        let a = key("agent-a", "grafana", PoolTransport::Stdio, "h1");
        let b = key("agent-a", "grafana", PoolTransport::Stdio, "h2");
        assert_ne!(a, b);
    }

    #[test]
    fn pool_key_same_inputs_match() {
        let a = key("agent-a", "grafana", PoolTransport::Stdio, "h");
        let b = key("agent-a", "grafana", PoolTransport::Stdio, "h");
        assert_eq!(a, b);
    }

    #[test]
    fn pool_key_debug_redacts_secret_fingerprint() {
        let key = key("agent-a", "grafana", PoolTransport::Stdio, "supersecret");
        let rendered = format!("{key:?}");
        assert!(rendered.contains("SecretFingerprint(<redacted>)"));
        assert!(
            !rendered.contains("supersecret"),
            "secret value leaked: {rendered}"
        );
    }
}
