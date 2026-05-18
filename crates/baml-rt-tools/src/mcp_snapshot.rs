//! Transport-independent serde types describing an approved MCP server import.
//!
//! These types are shared by the builder, runner, and runtime so they can
//! read snapshots without depending on any MCP protocol/transport crate.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    mcp_config::{
        HttpAuthConfig, McpServerConfig, McpServerTransportConfig, SecretInjection, SecretSource,
        StreamableHttpConfig,
    },
    tools::ToolAccess,
};

pub const MCP_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Top-level snapshot describing one MCP server import and the tools it exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSnapshot {
    pub schema_version: u32,
    pub server_id: String,
    pub transport: McpTransportRef,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<Value>,
    pub server_config_digest: Digest,
    /// Digest of the server-advertised identity contract captured at import
    /// time. Covers `capabilities` + `serverInfo.name` so cosmetic version
    /// bumps don't trip reconnect checks while binary swap / capability
    /// changes still fail closed.
    pub server_identity_digest: Digest,
    /// Digest over the full approved tool set: sorted
    /// `[(mcp_tool_name, input_schema_digest)]`. Runtime recomputes the same
    /// digest from a live `tools/list` during startup and after
    /// `notifications/tools/list_changed`, then marks the snapshot stale on
    /// mismatch.
    pub tools_digest: Digest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<SecretRef>,
    pub approval: ApprovalRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
    pub tools: Vec<McpImportedTool>,
}

/// Per-tool snapshot entry projected into platform tool metadata after approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpImportedTool {
    pub platform_tool_name: String,
    pub mcp_tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    pub input_schema_digest: Digest,
    pub output_mode: McpOutputMode,
    pub access_level: ToolAccess,
    pub approval: ApprovalRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub annotations: Value,
}

/// Approval lifecycle for a server or imported tool entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalState {
    Pending,
    Approved,
    Rejected,
    Stale,
}

impl McpApprovalState {
    pub fn is_approved(self) -> bool {
        matches!(self, McpApprovalState::Approved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub state: McpApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl ApprovalRecord {
    pub fn pending() -> Self {
        Self {
            state: McpApprovalState::Pending,
            owner: None,
            reviewed_at: None,
            expires_at: None,
        }
    }
}

/// Output shape selected at import time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpOutputMode {
    /// Stable content-block envelope; default when no validated output schema exists.
    ContentEnvelope,
    /// Raw opaque JSON wrapper for unsupported or weakly typed outputs.
    OpaqueJson,
    /// Output schema imported and digested.
    JsonSchema { schema: Value, digest: Digest },
}

/// Transport locator. Never carries raw secret values; only references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransportRef {
    /// Locally executed stdio MCP server. `command_ref` resolves to a vetted
    /// binary or artifact identifier outside the snapshot file.
    Stdio {
        command_ref: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
    /// Remote MCP server over MCP Streamable HTTP. Runtime support lands in
    /// the HTTP transport PRs; schema support lives here so approved snapshots
    /// can carry structured, secret-free config.
    StreamableHttp(StreamableHttpConfig),
}

/// Reference to a secret stored outside the snapshot. The runtime resolves
/// the actual value through the existing secret-resolver chain.
///
/// `source` / `inject` carry the canonical injection model from
/// `mcp_config::SecretSpec` so the snapshot records *how* the runtime
/// reconstructs the secret without persisting any value-derived material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SecretRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source: SecretSource,
    pub inject: SecretInjection,
}

impl SecretRef {
    /// Construct from a unified `SecretSpec` from `mcp_config`.
    pub fn from_spec(spec: &crate::mcp_config::SecretSpec) -> Self {
        Self {
            name: spec.id.clone(),
            version: spec.version.clone(),
            source: spec.source.clone(),
            inject: spec.inject.clone(),
        }
    }

    /// Convenience: stdio env-source/env-inject ref with matching name.
    pub fn stdio_env(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            version: None,
            source: SecretSource::Env { name: name.clone() },
            inject: SecretInjection::Env { name },
        }
    }
}

/// Newtype wrapper around a digest string. Format is implementation-defined
/// but conventionally `sha256:<hex>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hashes a value with JCS-canonicalised serde JSON then SHA-256, returning
/// the platform's `sha256:<hex>` digest. Centralises the digest convention
/// used by every MCP digest (server identity, tools, server config, snapshot
/// blob) so callers cannot drift on the canonicalisation or the prefix.
pub fn canonical_digest<T: Serialize + ?Sized>(value: &T) -> Digest {
    let bytes = serde_jcs::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Digest::new(format!("sha256:{:x}", hasher.finalize()))
}

/// Computes the server identity digest from the MCP `initialize` response.
///
/// Covers only the fields whose change must invalidate the existing
/// approval: server-advertised `capabilities` and `serverInfo.name`. The
/// cosmetic `serverInfo.version` and other implementation fields are
/// deliberately excluded so patch releases of an approved server do not
/// force re-approval.
///
/// `protocolVersion` is enforced by the client's pinned version during
/// `initialize`; a server that speaks a different revision cannot complete
/// the handshake, so including it here would be redundant.
pub fn compute_server_identity_digest(capabilities: &Value, server_info: &Value) -> Digest {
    let server_name = server_info.get("name").cloned().unwrap_or(Value::Null);
    let canonical = json!({
        "capabilities": capabilities,
        "server_info_name": server_name,
    });
    canonical_digest(&canonical)
}

/// Computes the server-wide tool-set digest from the imported tool list.
///
/// Covers the fields whose change must invalidate the approved contract:
/// the sorted set of MCP tool names and each tool's
/// `input_schema_digest`. Description and annotations are intentionally
/// excluded — they affect prompts, not the generated agent schema, and
/// would otherwise produce noisy drift signals on cosmetic upstream edits.
///
/// Importer, startup verification, and runtime drift handler must call this
/// with the same `McpImportedTool` projection so digests are comparable across
/// code paths.
pub fn compute_tools_digest(tools: &[McpImportedTool]) -> Digest {
    compute_tools_digest_from_entries(tools.iter().map(|tool| {
        (
            tool.mcp_tool_name.as_str(),
            tool.input_schema_digest.as_str(),
        )
    }))
}

/// Lower-level variant of [`compute_tools_digest`] for callers (e.g. the
/// startup/drift handler) that have `(mcp_tool_name, input_schema_digest)` pairs
/// without a full `McpImportedTool` projection.
pub fn compute_tools_digest_from_entries<'a, I>(entries: I) -> Digest
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut pairs: Vec<(&str, &str)> = entries.into_iter().collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    let canonical: Vec<Value> = pairs
        .into_iter()
        .map(|(name, digest)| json!({ "name": name, "input_schema_digest": digest }))
        .collect();
    canonical_digest(&canonical)
}

/// Computes the approved server config digest without embedding raw secret
/// values. For Streamable HTTP this covers transport kind, normalized URL,
/// protocol version, static non-secret headers, auth injection shape + secret
/// source identity, network policy, and approved tool scope digest.
///
/// **Stdio env policy:** non-secret `env` values (`GRAFANA_URL`,
/// `OPENAI_BASE_URL`, region/model selectors, etc.) are routing/identity
/// inputs and **are** folded into the digest — changing them must require
/// re-import/re-approval. Secret values never reach this map: the schema
/// layer (`McpServersFile::validate`) rejects env keys whose names look like
/// secrets, and resolved secrets are merged into the child process env at
/// runtime, not here.
pub fn compute_server_config_digest(
    server_id: &str,
    protocol_version: &str,
    config: &McpServerConfig,
    imported_tools_digest: Option<&Digest>,
) -> Digest {
    let tool_scope_digest = imported_tools_digest.map(Digest::as_str);
    let canonical = match &config.transport {
        Some(McpServerTransportConfig::StreamableHttp(http)) => json!({
            "server_id": server_id,
            "transport_kind": "streamable_http",
            "protocol_version": protocol_version,
            "url": normalized_http_url(&http.url),
            "headers": http.headers,
            "auth": auth_digest_shape(http.auth.as_ref()),
            "timeouts": http.timeouts,
            "pooling": http.pooling,
            "network_policy": effective_network_policy(&http.url, &http.network_policy),
            "imported_tools_digest": tool_scope_digest,
        }),
        None => json!({
            "server_id": server_id,
            "transport_kind": "stdio",
            "protocol_version": protocol_version,
            "command": config.command,
            "args": config.args,
            "env": &config.env,
            "secret_names": config.secrets.iter().map(|s| &s.name).collect::<Vec<_>>(),
            "sandbox": config.sandbox,
            "imported_tools_digest": tool_scope_digest,
        }),
    };
    canonical_digest(&canonical)
}

fn normalized_http_url(raw: &str) -> Value {
    match url::Url::parse(raw) {
        Ok(url) => json!({
            "scheme": url.scheme(),
            "host": url.host_str(),
            "port": url.port_or_known_default(),
            "path": url.path(),
        }),
        Err(_) => json!({ "raw": raw }),
    }
}

fn effective_network_policy(
    url: &str,
    policy: &crate::mcp_config::HttpNetworkPolicyConfig,
) -> Value {
    let allow_hosts = if policy.allow_hosts.is_empty() {
        url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        policy.allow_hosts.clone()
    };
    json!({
        "allow_hosts": allow_hosts,
        "allow_private_ips": policy.allow_private_ips,
        "follow_redirects": policy.follow_redirects,
    })
}

fn auth_digest_shape(auth: Option<&HttpAuthConfig>) -> Value {
    match auth {
        None => Value::Null,
        Some(HttpAuthConfig::Bearer { token_ref }) => json!({
            "kind": "bearer",
            "inject": { "kind": "http_header", "name": "Authorization", "scheme": "Bearer" },
            "secret_ref": token_ref,
        }),
        Some(HttpAuthConfig::Header { header, value_ref }) => json!({
            "kind": "header",
            "inject": { "kind": "http_header", "name": header },
            "secret_ref": value_ref,
        }),
        Some(HttpAuthConfig::Basic {
            username,
            password_ref,
        }) => json!({
            "kind": "basic",
            "inject": { "kind": "http_basic", "username": username },
            "secret_ref": password_ref,
        }),
    }
}

/// Returns true when the server and the named tool are both `Approved`.
/// Builder/runtime callers use this to decide whether a tool can be projected
/// into platform metadata.
pub fn is_tool_projectable(snapshot: &McpServerSnapshot, platform_tool_name: &str) -> bool {
    if !snapshot.approval.state.is_approved() {
        return false;
    }
    snapshot
        .tools
        .iter()
        .find(|tool| tool.platform_tool_name == platform_tool_name)
        .is_some_and(|tool| tool.approval.state.is_approved())
}

/// Iterator over tools whose per-tool approval is `Approved` when the server
/// itself is also `Approved`.
pub fn approved_tools(snapshot: &McpServerSnapshot) -> impl Iterator<Item = &McpImportedTool> {
    let server_approved = snapshot.approval.state.is_approved();
    snapshot
        .tools
        .iter()
        .filter(move |tool| server_approved && tool.approval.state.is_approved())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_tool(name: &str, state: McpApprovalState) -> McpImportedTool {
        McpImportedTool {
            platform_tool_name: format!("fake/{name}"),
            mcp_tool_name: name.to_string(),
            description: Some(format!("{name} description")),
            input_schema: json!({ "type": "object", "properties": {} }),
            input_schema_digest: Digest::new("sha256:input"),
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: ApprovalRecord {
                state,
                owner: Some("reviewer@example.com".into()),
                reviewed_at: Some("2026-05-13T00:00:00Z".into()),
                expires_at: None,
            },
            opaque_fallback_reason: None,
            annotations: Value::Null,
        }
    }

    fn sample_snapshot(state: McpApprovalState) -> McpServerSnapshot {
        McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: "fake-server".into(),
            transport: McpTransportRef::Stdio {
                command_ref: "fake-mcp".into(),
                args: vec![],
            },
            protocol_version: "2025-06-18".into(),
            server_info: Some(json!({ "name": "fake", "version": "0.1.0" })),
            server_config_digest: Digest::new("sha256:server-config"),
            server_identity_digest: Digest::new("sha256:server-identity"),
            tools_digest: Digest::new("sha256:tools"),
            secret_refs: vec![SecretRef::stdio_env("fake/api_token")],
            approval: ApprovalRecord {
                state,
                owner: Some("reviewer@example.com".into()),
                reviewed_at: Some("2026-05-13T00:00:00Z".into()),
                expires_at: None,
            },
            sandbox_profile: Some("mcp-import-restricted".into()),
            tools: vec![
                sample_tool("search", McpApprovalState::Approved),
                sample_tool("query", McpApprovalState::Approved),
            ],
        }
    }

    #[test]
    fn round_trip_minimal_approved_snapshot() {
        let snap = sample_snapshot(McpApprovalState::Approved);
        let json = serde_json::to_string(&snap).expect("serialize");
        let parsed: McpServerSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snap, parsed);
    }

    #[test]
    fn approval_variants_serialize_predictably() {
        for state in [
            McpApprovalState::Pending,
            McpApprovalState::Approved,
            McpApprovalState::Rejected,
            McpApprovalState::Stale,
        ] {
            let snap = sample_snapshot(state);
            let json = serde_json::to_value(&snap).expect("serialize");
            let serialized_state = json
                .pointer("/approval/state")
                .and_then(Value::as_str)
                .expect("state field present");
            let expected = match state {
                McpApprovalState::Pending => "pending",
                McpApprovalState::Approved => "approved",
                McpApprovalState::Rejected => "rejected",
                McpApprovalState::Stale => "stale",
            };
            assert_eq!(serialized_state, expected);
        }
    }

    #[test]
    fn projectable_requires_server_and_tool_approved() {
        let mut snap = sample_snapshot(McpApprovalState::Approved);
        assert!(is_tool_projectable(&snap, "fake/search"));

        snap.tools[0].approval.state = McpApprovalState::Pending;
        assert!(!is_tool_projectable(&snap, "fake/search"));
        assert!(is_tool_projectable(&snap, "fake/query"));

        let stale = sample_snapshot(McpApprovalState::Stale);
        assert!(!is_tool_projectable(&stale, "fake/search"));
    }

    #[test]
    fn approved_tools_excludes_unapproved_entries() {
        let mut snap = sample_snapshot(McpApprovalState::Approved);
        snap.tools
            .push(sample_tool("draft", McpApprovalState::Pending));
        let names: Vec<&str> = approved_tools(&snap)
            .map(|tool| tool.platform_tool_name.as_str())
            .collect();
        assert_eq!(names, vec!["fake/search", "fake/query"]);
    }

    #[test]
    fn approved_tools_empty_when_server_not_approved() {
        let snap = sample_snapshot(McpApprovalState::Rejected);
        assert_eq!(approved_tools(&snap).count(), 0);
    }

    #[test]
    fn snapshot_serialization_contains_no_raw_secret_fields() {
        let snap = sample_snapshot(McpApprovalState::Approved);
        let json = serde_json::to_string(&snap).expect("serialize");
        for forbidden in [
            "\"token\"",
            "\"password\"",
            "\"api_key\"",
            "\"secret_value\"",
            "\"credential\"",
        ] {
            assert!(
                !json.contains(forbidden),
                "snapshot contains forbidden secret field {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn output_mode_variants_round_trip() {
        let envelope = McpOutputMode::ContentEnvelope;
        let opaque = McpOutputMode::OpaqueJson;
        let schema = McpOutputMode::JsonSchema {
            schema: json!({ "type": "object" }),
            digest: Digest::new("sha256:out"),
        };
        for mode in [envelope, opaque, schema] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let parsed: McpOutputMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn transport_ref_variants_round_trip() {
        let stdio = McpTransportRef::Stdio {
            command_ref: "fake-mcp".into(),
            args: vec!["--quiet".into()],
        };
        let http = McpTransportRef::StreamableHttp(StreamableHttpConfig {
            url: "https://example.invalid/mcp".into(),
            headers: vec![],
            auth: None,
            timeouts: Default::default(),
            pooling: Default::default(),
            network_policy: Default::default(),
        });
        for transport in [stdio, http] {
            let json = serde_json::to_string(&transport).expect("serialize");
            let parsed: McpTransportRef = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(transport, parsed);
        }
    }

    #[test]
    fn streamable_http_snapshot_serialization_contains_no_raw_secret_fields() {
        let transport = McpTransportRef::StreamableHttp(StreamableHttpConfig {
            url: "https://example.invalid/mcp".into(),
            headers: vec![crate::mcp_config::HttpHeader {
                name: "X-Client-Name".into(),
                value: "agent-platform".into(),
            }],
            auth: Some(HttpAuthConfig::Header {
                header: "X-API-Key".into(),
                value_ref: crate::mcp_config::HttpSecretRef {
                    source: crate::mcp_config::SecretSource::Env {
                        name: "API_KEY".into(),
                    },
                },
            }),
            timeouts: Default::default(),
            pooling: Default::default(),
            network_policy: Default::default(),
        });
        let json = serde_json::to_string_pretty(&transport).expect("serialize");
        assert!(json.contains("streamable_http"));
        assert!(json.contains("API_KEY"));
        for forbidden in [
            "RAW_SECRET_CANARY",
            "secret_value",
            "password_value",
            "token_value",
        ] {
            assert!(!json.contains(forbidden), "leaked {forbidden}: {json}");
        }
    }

    #[test]
    fn server_config_digest_tracks_streamable_http_shape_not_secret_value() {
        let mut config = McpServerConfig {
            transport: Some(McpServerTransportConfig::StreamableHttp(
                StreamableHttpConfig {
                    url: "https://example.invalid:443/mcp".into(),
                    headers: vec![crate::mcp_config::HttpHeader {
                        name: "X-Client-Name".into(),
                        value: "agent-platform".into(),
                    }],
                    auth: Some(HttpAuthConfig::Bearer {
                        token_ref: crate::mcp_config::HttpSecretRef {
                            source: crate::mcp_config::SecretSource::Env {
                                name: "GRAFANA_TOKEN".into(),
                            },
                        },
                    }),
                    timeouts: Default::default(),
                    pooling: Default::default(),
                    network_policy: crate::mcp_config::HttpNetworkPolicyConfig {
                        allow_hosts: vec!["example.invalid".into()],
                        allow_private_ips: false,
                        follow_redirects: false,
                    },
                },
            )),
            command: String::new(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            secrets: vec![],
            sandbox: None,
            description: None,
        };
        let tools_a = Digest::new("sha256:tools-a");
        let a = compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_a));
        let b = compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_a));
        assert_eq!(a, b, "raw secret values are not an input, only refs are");

        with_streamable_http_mut(&mut config, |http| {
            http.url = "https://other.invalid/mcp".into();
        });
        let changed_url =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_a));
        assert_ne!(a, changed_url);

        with_streamable_http_mut(&mut config, |http| {
            http.url = "https://example.invalid/mcp".into();
            http.auth = Some(HttpAuthConfig::Basic {
                username: "grafana".into(),
                password_ref: crate::mcp_config::HttpSecretRef {
                    source: crate::mcp_config::SecretSource::Env {
                        name: "GRAFANA_PASSWORD".into(),
                    },
                },
            });
        });
        let changed_auth =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_a));
        assert_ne!(a, changed_auth);

        with_streamable_http_mut(&mut config, |http| {
            http.auth = Some(HttpAuthConfig::Bearer {
                token_ref: crate::mcp_config::HttpSecretRef {
                    source: crate::mcp_config::SecretSource::Env {
                        name: "GRAFANA_TOKEN".into(),
                    },
                },
            });
        });
        let tools_b = Digest::new("sha256:tools-b");
        let changed_tools =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_b));
        assert_ne!(a, changed_tools);

        let changed_protocol =
            compute_server_config_digest("grafana", "2099-01-01", &config, Some(&tools_a));
        assert_ne!(a, changed_protocol);

        with_streamable_http_mut(&mut config, |http| {
            http.network_policy = crate::mcp_config::HttpNetworkPolicyConfig {
                allow_hosts: vec!["other.invalid".into()],
                allow_private_ips: false,
                follow_redirects: false,
            };
        });
        let changed_policy =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools_a));
        assert_ne!(a, changed_policy);
    }

    fn with_streamable_http_mut(
        config: &mut McpServerConfig,
        f: impl FnOnce(&mut StreamableHttpConfig),
    ) {
        let Some(McpServerTransportConfig::StreamableHttp(http)) = &mut config.transport else {
            panic!("expected streamable_http");
        };
        f(http);
    }

    #[test]
    fn stdio_digest_folds_in_non_secret_env_values() {
        // Routing env values (URL, region, model id) are part of the
        // governed identity. Changing one must force re-import.
        let mut config = McpServerConfig {
            transport: None,
            command: "grafana-mcp".into(),
            args: vec![],
            env: std::collections::BTreeMap::from([(
                "GRAFANA_URL".into(),
                "https://grafana.example/api".into(),
            )]),
            secrets: vec![],
            sandbox: None,
            description: None,
        };
        let tools = Digest::new("sha256:tools");
        let base = compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools));

        // Same input → same digest.
        let same = compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools));
        assert_eq!(base, same);

        // Routing value changed → digest must change.
        config
            .env
            .insert("GRAFANA_URL".into(), "https://grafana.other/api".into());
        let changed_url =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools));
        assert_ne!(base, changed_url, "non-secret env value must affect digest");

        // New non-secret env key added → digest must change.
        config
            .env
            .insert("GRAFANA_URL".into(), "https://grafana.example/api".into());
        config
            .env
            .insert("GRAFANA_REGION".into(), "us-east-1".into());
        let added_key =
            compute_server_config_digest("grafana", "2025-06-18", &config, Some(&tools));
        assert_ne!(base, added_key, "new non-secret env key must affect digest");
    }

    #[test]
    fn digest_stable_for_same_input() {
        let a = Digest::new("sha256:abc");
        let b = Digest::new("sha256:abc");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "sha256:abc");
    }
}
