//! Importer: spawns an MCP server in a Tier 1 sandbox, runs discovery,
//! and produces a pending `McpServerSnapshot`.
//!
//! Approval is a separate concern; the importer never writes the snapshot
//! in `approved` state.

use std::{collections::BTreeMap, time::Duration};

use baml_rt_tools::{
    mcp_config::{McpServerConfig, McpServerTransportConfig, SecretDecl},
    mcp_schema_normalize::normalize,
    mcp_snapshot::{
        ApprovalRecord, MCP_SNAPSHOT_SCHEMA_VERSION, McpImportedTool, McpOutputMode,
        McpServerSnapshot, McpTransportRef, SecretRef, compute_server_config_digest,
        compute_server_identity_digest, compute_tools_digest,
    },
    tools::ToolAccess,
};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::{
    client::{CLIENT_PROTOCOL_VERSION, McpRpcError, McpStdioClient, ToolDescriptor},
    sandbox::{SandboxError, SandboxedChild, SpawnSpec, spawn as sandbox_spawn},
};

const DEFAULT_IMPORT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("sandbox: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("rpc: {0}")]
    Rpc(#[from] McpRpcError),
    #[error("server returned no stdout/stdin handles")]
    NoChildIo,
    #[error("server advertised protocol version `{advertised}`, importer expects `{expected}`")]
    ProtocolMismatch {
        advertised: String,
        expected: String,
    },
    #[error("server `{server_id}` exposes no tools")]
    NoTools { server_id: String },
    #[error("MCP import for `{kind}` transport is not implemented in this build")]
    UnsupportedTransport { kind: &'static str },
    #[error("missing secret value for `{name}` (resolver returned no value)")]
    MissingSecret { name: String },
}

/// How the caller wants secrets resolved for the child process.
pub trait SecretResolver {
    /// Returns the resolved value for the named secret, or `None` if absent.
    /// Implementations should not log raw values.
    fn resolve(&self, name: &str) -> Option<String>;
}

/// Resolver that reads from process env. Useful for tests and as the
/// outermost fallback in production resolver chains.
pub struct EnvSecretResolver;

impl SecretResolver for EnvSecretResolver {
    fn resolve(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub server_id: String,
    pub sandbox_profile: Option<String>,
}

pub struct Importer<'a, R: SecretResolver + Sync> {
    pub resolver: &'a R,
}

impl<'a, R: SecretResolver + Sync> Importer<'a, R> {
    pub fn new(resolver: &'a R) -> Self {
        Self { resolver }
    }

    /// Spawn the MCP server in a Tier 1 sandbox, run discovery, and assemble
    /// a `McpServerSnapshot` in `pending` approval state.
    pub async fn import(
        &self,
        config: &McpServerConfig,
        options: ImportOptions,
    ) -> Result<McpServerSnapshot, ImportError> {
        if matches!(
            config.transport,
            Some(McpServerTransportConfig::StreamableHttp(_))
        ) {
            return Err(ImportError::UnsupportedTransport {
                kind: "streamable_http",
            });
        }
        let env = resolve_env(config, self.resolver)?;
        let timeout = Duration::from_secs(
            config
                .sandbox
                .as_ref()
                .and_then(|s| s.import_timeout_secs)
                .unwrap_or(DEFAULT_IMPORT_TIMEOUT_SECS),
        );
        let spec = SpawnSpec {
            command: config.command.clone(),
            args: config.args.clone(),
            env,
            timeout,
        };

        let mut sandboxed = sandbox_spawn(spec)?;
        let (initialize_result, descriptors, stderr_tail) =
            run_discovery(&mut sandboxed, timeout).await?;

        if initialize_result.protocol_version != CLIENT_PROTOCOL_VERSION {
            // The pinned protocol version is load-bearing for digest
            // contracts: a server speaking a different revision may produce
            // schemas whose canonical form differs from what was approved.
            // Fail closed at import time rather than silently approving
            // mismatched contracts.
            return Err(ImportError::ProtocolMismatch {
                advertised: initialize_result.protocol_version,
                expected: CLIENT_PROTOCOL_VERSION.to_string(),
            });
        }

        if descriptors.is_empty() {
            return Err(ImportError::NoTools {
                server_id: options.server_id.clone(),
            });
        }

        let tools = project_tools(&options.server_id, descriptors)?;
        let tools_digest = compute_tools_digest(&tools);
        let server_config_digest = compute_server_config_digest(
            &options.server_id,
            CLIENT_PROTOCOL_VERSION,
            config,
            Some(&tools_digest),
        );
        let server_identity_digest = compute_server_identity_digest(
            &initialize_result.capabilities,
            &initialize_result.server_info,
        );
        // Derive snapshot secret refs from the unified `SecretSpec` view so
        // every transport records `source` + `inject` consistently. Streamable
        // HTTP import is still rejected above, but this keeps the projection
        // in one place for the runtime resolver.
        let secret_refs = config
            .secret_specs()
            .iter()
            .map(SecretRef::from_spec)
            .collect();

        Ok(McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: options.server_id,
            transport: McpTransportRef::Stdio {
                command_ref: config.command.clone(),
                args: config.args.clone(),
            },
            protocol_version: initialize_result.protocol_version,
            server_info: Some(initialize_result.server_info),
            server_config_digest,
            server_identity_digest,
            tools_digest,
            secret_refs,
            approval: ApprovalRecord::pending(),
            sandbox_profile: options
                .sandbox_profile
                .or_else(|| config.sandbox.as_ref().and_then(|s| s.profile.clone()))
                .or_else(|| Some(DEFAULT_SANDBOX_PROFILE.to_string())),
            tools,
        })
        .map(|mut snapshot| {
            if !stderr_tail.is_empty() {
                tracing::debug!(stderr_tail = %stderr_tail, "captured MCP stderr during import");
            }
            // Stable tool ordering on disk.
            snapshot
                .tools
                .sort_by(|a, b| a.platform_tool_name.cmp(&b.platform_tool_name));
            snapshot
        })
    }
}

const DEFAULT_SANDBOX_PROFILE: &str = "mcp-import-restricted-tier1";

async fn run_discovery(
    sandboxed: &mut SandboxedChild,
    timeout: Duration,
) -> Result<(crate::client::InitializeResult, Vec<ToolDescriptor>, String), ImportError> {
    let stdin = sandboxed.child.stdin.take().ok_or(ImportError::NoChildIo)?;
    let stdout = sandboxed
        .child
        .stdout
        .take()
        .ok_or(ImportError::NoChildIo)?;
    let mut stderr = sandboxed.child.stderr.take();
    let mut client = McpStdioClient::new(stdin, stdout);

    let initialize_result = client.initialize(timeout).await?;
    let tools = client.list_tools(timeout).await?.tools;

    drop(client);

    let mut stderr_tail = String::new();
    if let Some(mut stream) = stderr.take() {
        match tokio::time::timeout(
            Duration::from_millis(250),
            stream.read_to_string(&mut stderr_tail),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => tracing::warn!(
                target: "mcp.importer",
                error = %err,
                event = "mcp.import_stderr_drain_failed",
                "failed to drain MCP child stderr after discovery"
            ),
            Err(_) => tracing::debug!(
                target: "mcp.importer",
                event = "mcp.import_stderr_drain_timeout",
                "MCP child stderr drain timed out (250ms cap)"
            ),
        }
    }

    // Signal the child to exit now that discovery is done. `kill_on_drop`
    // would do this eventually, but only when `Drop` runs in the reaper —
    // for k8s SIGKILL or async-drop panics that is never. Best-effort.
    if let Err(err) = sandboxed.child.start_kill() {
        tracing::debug!(
            target: "mcp.importer",
            error = %err,
            event = "mcp.import_child_kill_failed",
            "failed to start_kill MCP import child; will rely on kill_on_drop"
        );
    }

    Ok((initialize_result, tools, stderr_tail))
}

fn resolve_env<R: SecretResolver + ?Sized>(
    config: &McpServerConfig,
    resolver: &R,
) -> Result<BTreeMap<String, String>, ImportError> {
    let mut env: BTreeMap<String, String> = config.env.clone();
    if let Ok(path) = std::env::var("PATH") {
        // Re-inject PATH so `uvx`, `npx`, system interpreters remain discoverable
        // inside the env_clear'd child. Operator can override via explicit `env`.
        env.entry("PATH".into()).or_insert(path);
    }
    for SecretDecl { name, .. } in &config.secrets {
        let value = resolver
            .resolve(name)
            .ok_or_else(|| ImportError::MissingSecret { name: name.clone() })?;
        env.insert(name.clone(), value);
    }
    Ok(env)
}

fn project_tools(
    server_id: &str,
    descriptors: Vec<ToolDescriptor>,
) -> Result<Vec<McpImportedTool>, ImportError> {
    let mut out = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let normalized = normalize(&descriptor.input_schema);
        let tool = McpImportedTool {
            platform_tool_name: format!("mcp/{server_id}/{}", descriptor.name),
            mcp_tool_name: descriptor.name,
            description: descriptor.description,
            input_schema: normalized.schema,
            input_schema_digest: normalized.digest,
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: ApprovalRecord::pending(),
            opaque_fallback_reason: normalized.opaque_fallback_reason,
            annotations: descriptor.annotations,
        };
        out.push(tool);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::{
        mcp_config::{SandboxConfig, SecretDecl},
        mcp_snapshot::Digest,
    };

    use super::*;

    struct MapResolver(std::collections::HashMap<String, String>);

    impl SecretResolver for MapResolver {
        fn resolve(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn server_config_digest_tracks_non_secret_env_values() {
        // Non-secret env values are routing/identity inputs (URL, region,
        // model id) and must be governed: changing one forces re-import.
        // Secret values never reach `config.env` — the schema layer rejects
        // env keys that look like secrets, and resolved secrets are merged
        // into the child process env at runtime, not at digest time.
        let mut config = McpServerConfig {
            transport: None,
            command: "uvx".into(),
            args: vec!["mcp-grafana".into()],
            env: BTreeMap::from([("GRAFANA_URL".into(), "http://x".into())]),
            secrets: vec![SecretDecl {
                name: "TOKEN".into(),
                description: None,
                reason: None,
            }],
            sandbox: Some(SandboxConfig::default()),
            description: None,
        };
        let tools = Digest::new("sha256:tools");
        let a =
            compute_server_config_digest("grafana", CLIENT_PROTOCOL_VERSION, &config, Some(&tools));
        // Same input → same digest.
        let a2 =
            compute_server_config_digest("grafana", CLIENT_PROTOCOL_VERSION, &config, Some(&tools));
        assert_eq!(a, a2);
        // Changing a non-secret env value MUST shift the digest so operator
        // re-approval is required.
        config
            .env
            .insert("GRAFANA_URL".into(), "http://other".into());
        let b =
            compute_server_config_digest("grafana", CLIENT_PROTOCOL_VERSION, &config, Some(&tools));
        assert_ne!(a, b, "non-secret env value change must shift digest");
        // Adding a new key also changes digest.
        config.env.insert("EXTRA".into(), "y".into());
        let c =
            compute_server_config_digest("grafana", CLIENT_PROTOCOL_VERSION, &config, Some(&tools));
        assert_ne!(b, c);
    }

    #[test]
    fn missing_secret_is_reported() {
        let cfg = McpServerConfig {
            transport: None,
            command: "sh".into(),
            args: vec!["-c".into(), ":".into()],
            env: BTreeMap::new(),
            secrets: vec![SecretDecl {
                name: "MISSING".into(),
                description: None,
                reason: None,
            }],
            sandbox: None,
            description: None,
        };
        let resolver = MapResolver(std::collections::HashMap::new());
        let err = resolve_env(&cfg, &resolver).unwrap_err();
        assert!(matches!(err, ImportError::MissingSecret { .. }));
    }

    #[test]
    fn project_tools_marks_unsupported_input_for_opaque_fallback() {
        let descriptors = vec![ToolDescriptor {
            name: "search".into(),
            description: Some("desc".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "x": { "$ref": "#/Foo" } }
            }),
            annotations: serde_json::Value::Null,
        }];
        let tools = project_tools("fake", descriptors).unwrap();
        assert_eq!(tools[0].platform_tool_name, "mcp/fake/search");
        assert!(tools[0].opaque_fallback_reason.is_some());
    }
}
