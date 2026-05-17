//! Operator-owned `mcp-servers.json` config: declares MCP servers a runner
//! instance may import or run.
//!
//! Accepts the canonical `mcpServers` shape used by Claude Desktop and similar
//! MCP clients, extended with `secrets` and `sandbox` blocks. Raw secret values
//! are not permitted in `env`; declare them in `secrets` and the runtime
//! resolves them through the existing fnox/env chain at child-spawn time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersFile {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Omitted for legacy/Claude Desktop stdio configs. Present for transport
    /// variants with structured config such as Streamable HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpServerTransportConfig>,
    /// Stdio command. Required when `transport` is absent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Plain environment variables. Must not carry secret values; use
    /// `secrets` instead.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Required secrets, declared the same way `#[baml_tool]` tools declare
    /// theirs. The runtime resolves each `name` via the configured secret
    /// resolver chain and injects the value into the child process env.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,
    /// Optional human-readable description shown by MCP registry import commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpServerTransportConfig {
    StreamableHttp(StreamableHttpConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamableHttpConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HttpHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<HttpAuthConfig>,
    #[serde(default, skip_serializing_if = "HttpTimeoutsConfig::is_default")]
    pub timeouts: HttpTimeoutsConfig,
    #[serde(default, skip_serializing_if = "HttpPoolingConfig::is_default")]
    pub pooling: HttpPoolingConfig,
    #[serde(default, skip_serializing_if = "HttpNetworkPolicyConfig::is_default")]
    pub network_policy: HttpNetworkPolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HttpAuthConfig {
    Bearer {
        token_ref: HttpSecretRef,
    },
    Header {
        header: String,
        value_ref: HttpSecretRef,
    },
    Basic {
        username: String,
        password_ref: HttpSecretRef,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct HttpSecretRef {
    pub source: SecretSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretSource {
    Env { name: String },
}

impl SecretSource {
    /// Stable identity token used in pool keys and logs. Never contains the
    /// resolved value. Phase 2 only supports `env`; the prefix discriminates
    /// future sources so a future `vault:` source can't collide with an `env:`
    /// source that happens to share a name.
    pub fn identity(&self) -> String {
        match self {
            SecretSource::Env { name } => format!("env:{name}"),
        }
    }
}

/// Injection target. Closed by construction: query-string, path-segment, and
/// cookie injection (all forbidden in Phase 2) are not representable.
/// Username-as-secret (also forbidden) is not representable either —
/// `HttpBasicPassword` carries a literal `username`, not a ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretInjection {
    /// Stdio: inject as child-process env variable `name`.
    Env { name: String },
    /// Streamable HTTP: inject as `name: <value>` request header.
    HttpHeader { name: String },
    /// Streamable HTTP: inject as `Authorization: Bearer <value>`.
    HttpAuthorizationBearer,
    /// Streamable HTTP: inject as `Authorization: Basic base64(username:value)`.
    HttpBasicPassword { username: String },
}

/// Canonical, transport-agnostic description of one secret a server needs.
///
/// `id` is server-scoped and stable across runs; used as the key in the
/// resolved-value map. `source` describes where the runtime looks up the
/// value; `inject` describes where the runtime puts the resolved value.
///
/// Built from operator config via [`McpServerConfig::secret_specs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSpec {
    pub id: String,
    pub source: SecretSource,
    pub inject: SecretInjection,
    pub version: Option<String>,
}

impl SecretSpec {
    /// Stable identity+version token. Hashes/pool keys built from this token
    /// change on credential rotation (new version) and on source rebind, but
    /// never depend on the resolved secret value.
    pub fn identity_token(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{v}", self.source.identity()),
            None => self.source.identity(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpTimeoutsConfig {
    #[serde(default = "default_connect_ms")]
    pub connect_ms: u64,
    #[serde(default = "default_request_ms")]
    pub request_ms: u64,
    #[serde(default = "default_idle_stream_ms")]
    pub idle_stream_ms: u64,
}

impl Default for HttpTimeoutsConfig {
    fn default() -> Self {
        Self {
            connect_ms: default_connect_ms(),
            request_ms: default_request_ms(),
            idle_stream_ms: default_idle_stream_ms(),
        }
    }
}

impl HttpTimeoutsConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpPoolingConfig {
    #[serde(default = "default_share_safe")]
    pub share_safe: bool,
    #[serde(default = "default_max_idle_per_host")]
    pub max_idle_per_host: u64,
    #[serde(default = "default_max_concurrent_requests_per_pool_key")]
    pub max_concurrent_requests_per_pool_key: u64,
    #[serde(default = "default_max_concurrent_requests_per_agent")]
    pub max_concurrent_requests_per_agent: u64,
    #[serde(default = "default_idle_ttl_ms")]
    pub idle_ttl_ms: u64,
}

impl Default for HttpPoolingConfig {
    fn default() -> Self {
        Self {
            share_safe: default_share_safe(),
            max_idle_per_host: default_max_idle_per_host(),
            max_concurrent_requests_per_pool_key: default_max_concurrent_requests_per_pool_key(),
            max_concurrent_requests_per_agent: default_max_concurrent_requests_per_agent(),
            idle_ttl_ms: default_idle_ttl_ms(),
        }
    }
}

impl HttpPoolingConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpNetworkPolicyConfig {
    /// Empty means "derive allowlist from URL host".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_hosts: Vec<String>,
    #[serde(default = "default_allow_private_ips")]
    pub allow_private_ips: bool,
    #[serde(default = "default_follow_redirects")]
    pub follow_redirects: bool,
}

impl Default for HttpNetworkPolicyConfig {
    fn default() -> Self {
        Self {
            allow_hosts: vec![],
            allow_private_ips: default_allow_private_ips(),
            follow_redirects: default_follow_redirects(),
        }
    }
}

impl HttpNetworkPolicyConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

const fn default_connect_ms() -> u64 {
    5_000
}

const fn default_request_ms() -> u64 {
    60_000
}

const fn default_idle_stream_ms() -> u64 {
    30_000
}

const fn default_share_safe() -> bool {
    false
}

const fn default_max_idle_per_host() -> u64 {
    8
}

const fn default_max_concurrent_requests_per_pool_key() -> u64 {
    16
}

const fn default_max_concurrent_requests_per_agent() -> u64 {
    4
}

const fn default_idle_ttl_ms() -> u64 {
    300_000
}

const fn default_allow_private_ips() -> bool {
    false
}

const fn default_follow_redirects() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxConfig {
    /// Sandbox profile name. Defaults to `mcp-import-restricted` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Hard deadline (seconds) for import-time discovery. Defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_timeout_secs: Option<u64>,
    /// Per-call deadline (seconds) for `tools/call` at runtime. Defaults to 120.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_call_timeout_secs: Option<u64>,
}

#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error("invalid mcp-servers.json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(
        "server `{server}` env field `{key}` looks like a secret; declare it under `secrets` instead"
    )]
    SecretInEnv { server: String, key: String },
    #[error("server `{server}` declares secret `{name}` more than once")]
    DuplicateSecret { server: String, name: String },
    #[error(
        "server id `{0}` must be non-empty and contain only ASCII letters, digits, `_`, or `-`"
    )]
    InvalidServerId(String),
    #[error("server `{server}` command is empty")]
    EmptyCommand { server: String },
    #[error("server `{server}` streamable_http url is invalid: {reason}")]
    InvalidHttpUrl { server: String, reason: String },
    #[error("server `{server}` streamable_http header name is empty")]
    EmptyHttpHeaderName { server: String },
    #[error("server `{server}` streamable_http auth header name is empty")]
    EmptyHttpAuthHeaderName { server: String },
}

impl McpServerConfig {
    /// Normalize the operator config's secret needs into a transport-agnostic
    /// list of `SecretSpec`. Importer, resolver, and runtime all consume this
    /// shape so injection-site changes never re-parse raw config.
    ///
    /// Stdio: each `SecretDecl { name }` becomes an env-source/env-inject
    /// spec with `id = name`.
    ///
    /// Streamable HTTP auth helpers compile to:
    ///   - `bearer` -> `HttpAuthorizationBearer`
    ///   - `header` -> `HttpHeader { name }`
    ///   - `basic`  -> `HttpBasicPassword { username }`
    ///
    /// Order is deterministic: stdio `secrets` first (declaration order),
    /// then HTTP auth.
    pub fn secret_specs(&self) -> Vec<SecretSpec> {
        let mut specs = Vec::new();
        for decl in &self.secrets {
            specs.push(SecretSpec {
                id: decl.name.clone(),
                source: SecretSource::Env {
                    name: decl.name.clone(),
                },
                inject: SecretInjection::Env {
                    name: decl.name.clone(),
                },
                version: None,
            });
        }
        if let Some(McpServerTransportConfig::StreamableHttp(http)) = &self.transport
            && let Some(auth) = &http.auth
        {
            match auth {
                HttpAuthConfig::Bearer { token_ref } => specs.push(SecretSpec {
                    id: "auth.bearer".to_string(),
                    source: token_ref.source.clone(),
                    inject: SecretInjection::HttpAuthorizationBearer,
                    version: None,
                }),
                HttpAuthConfig::Header { header, value_ref } => specs.push(SecretSpec {
                    id: format!("auth.header.{header}"),
                    source: value_ref.source.clone(),
                    inject: SecretInjection::HttpHeader {
                        name: header.clone(),
                    },
                    version: None,
                }),
                HttpAuthConfig::Basic {
                    username,
                    password_ref,
                } => specs.push(SecretSpec {
                    id: format!("auth.basic.{username}"),
                    source: password_ref.source.clone(),
                    inject: SecretInjection::HttpBasicPassword {
                        username: username.clone(),
                    },
                    version: None,
                }),
            }
        }
        specs
    }
}

impl McpServersFile {
    pub fn parse(input: &str) -> Result<Self, McpConfigError> {
        let parsed: Self = serde_json::from_str(input)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), McpConfigError> {
        for (id, config) in &self.servers {
            if !is_valid_server_id(id) {
                return Err(McpConfigError::InvalidServerId(id.clone()));
            }
            match &config.transport {
                None if config.command.trim().is_empty() => {
                    return Err(McpConfigError::EmptyCommand { server: id.clone() });
                }
                Some(McpServerTransportConfig::StreamableHttp(http)) => {
                    validate_streamable_http_config(id, http)?;
                }
                None => {}
            }
            for key in config.env.keys() {
                if looks_like_secret_name(key) {
                    return Err(McpConfigError::SecretInEnv {
                        server: id.clone(),
                        key: key.clone(),
                    });
                }
            }
            let mut seen = std::collections::HashSet::new();
            for secret in &config.secrets {
                if !seen.insert(secret.name.clone()) {
                    return Err(McpConfigError::DuplicateSecret {
                        server: id.clone(),
                        name: secret.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_streamable_http_config(
    server: &str,
    config: &StreamableHttpConfig,
) -> Result<(), McpConfigError> {
    let parsed = Url::parse(&config.url).map_err(|err| McpConfigError::InvalidHttpUrl {
        server: server.to_string(),
        reason: err.to_string(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(McpConfigError::InvalidHttpUrl {
            server: server.to_string(),
            reason: "url must be absolute http(s) URL with host".into(),
        });
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(McpConfigError::InvalidHttpUrl {
            server: server.to_string(),
            reason: "url must not contain embedded credentials".into(),
        });
    }
    if parsed.query().is_some() {
        return Err(McpConfigError::InvalidHttpUrl {
            server: server.to_string(),
            reason: "url must not contain query parameters".into(),
        });
    }
    for header in &config.headers {
        if header.name.trim().is_empty() {
            return Err(McpConfigError::EmptyHttpHeaderName {
                server: server.to_string(),
            });
        }
    }
    if let Some(HttpAuthConfig::Header { header, .. }) = &config.auth
        && header.trim().is_empty()
    {
        return Err(McpConfigError::EmptyHttpAuthHeaderName {
            server: server.to_string(),
        });
    }
    Ok(())
}

fn is_valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Conservative heuristic: env keys containing these tokens are rejected as
/// likely secret carriers. Operators that need a literal env var with one of
/// these names can rename it (e.g. set the secret via the `secrets:` block
/// and reference it from inside the MCP server itself).
fn looks_like_secret_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_mcp_servers_shape() {
        let json = r#"{
          "mcpServers": {
            "grafana": {
              "command": "uvx",
              "args": ["mcp-grafana"],
              "env": { "GRAFANA_URL": "http://localhost:3000" },
              "secrets": [
                { "name": "GRAFANA_SERVICE_ACCOUNT_TOKEN", "description": "Grafana SA token" }
              ],
              "sandbox": { "profile": "mcp-import-restricted" }
            }
          }
        }"#;
        let parsed = McpServersFile::parse(json).expect("parse");
        let server = parsed.servers.get("grafana").expect("server present");
        assert!(server.transport.is_none());
        assert_eq!(server.command, "uvx");
        assert_eq!(server.args, vec!["mcp-grafana".to_string()]);
        assert_eq!(
            server.env.get("GRAFANA_URL").map(String::as_str),
            Some("http://localhost:3000")
        );
        assert_eq!(server.secrets.len(), 1);
        assert_eq!(server.secrets[0].name, "GRAFANA_SERVICE_ACCOUNT_TOKEN");
    }

    #[test]
    fn rejects_secret_looking_env_key() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "command": "foo",
              "env": { "FOO_API_KEY": "abc" }
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("should reject");
        assert!(matches!(err, McpConfigError::SecretInEnv { .. }));
    }

    #[test]
    fn rejects_empty_command() {
        let json = r#"{ "mcpServers": { "x": { "command": "" } } }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::EmptyCommand { .. }));
    }

    #[test]
    fn rejects_invalid_server_id() {
        let json = r#"{ "mcpServers": { "bad id!": { "command": "x" } } }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::InvalidServerId(_)));
    }

    #[test]
    fn rejects_duplicate_secret_name() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "command": "foo",
              "secrets": [
                { "name": "A" },
                { "name": "A" }
              ]
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::DuplicateSecret { .. }));
    }

    #[test]
    fn parses_streamable_http_transport_shape() {
        let json = r#"{
          "mcpServers": {
            "grafana": {
              "transport": {
                "kind": "streamable_http",
                "url": "https://mcp.grafana.example.com/mcp",
                "headers": [
                  { "name": "X-Client-Name", "value": "agent-platform" }
                ],
                "auth": {
                  "kind": "bearer",
                  "token_ref": { "source": { "kind": "env", "name": "GRAFANA_TOKEN" } }
                },
                "timeouts": {
                  "connect_ms": 5000,
                  "request_ms": 60000,
                  "idle_stream_ms": 30000
                },
                "pooling": {
                  "share_safe": true,
                  "max_idle_per_host": 8,
                  "max_concurrent_requests_per_pool_key": 32,
                  "max_concurrent_requests_per_agent": 8,
                  "idle_ttl_ms": 300000
                },
                "network_policy": {
                  "allow_hosts": ["mcp.grafana.example.com"],
                  "allow_private_ips": false,
                  "follow_redirects": false
                }
              }
            }
          }
        }"#;
        let parsed = McpServersFile::parse(json).expect("parse");
        let server = parsed.servers.get("grafana").expect("server present");
        let Some(McpServerTransportConfig::StreamableHttp(http)) = &server.transport else {
            panic!("expected streamable_http transport");
        };
        assert_eq!(http.url, "https://mcp.grafana.example.com/mcp");
        assert_eq!(http.headers[0].name, "X-Client-Name");
        assert!(matches!(http.auth, Some(HttpAuthConfig::Bearer { .. })));
        assert_eq!(http.timeouts.connect_ms, 5000);
        assert!(http.pooling.share_safe);
        assert_eq!(
            http.network_policy.allow_hosts,
            vec!["mcp.grafana.example.com"]
        );
        let json = serde_json::to_string(server).expect("serialize");
        assert!(json.contains("streamable_http"));
        assert!(!json.contains("GRAFANA_TOKEN_VALUE_CANARY"));
    }

    #[test]
    fn rejects_streamable_http_url_credentials() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "transport": {
                "kind": "streamable_http",
                "url": "https://user:pass@example.com/mcp"
              }
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::InvalidHttpUrl { .. }));
    }

    #[test]
    fn rejects_streamable_http_url_query_params() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "transport": {
                "kind": "streamable_http",
                "url": "https://example.com/mcp?token=abc"
              }
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::InvalidHttpUrl { .. }));
        assert!(!err.to_string().contains("abc"));
    }

    #[test]
    fn secret_specs_stdio_maps_decls_to_env_inject() {
        let cfg = McpServerConfig {
            transport: None,
            command: "uvx".into(),
            args: vec![],
            env: BTreeMap::new(),
            secrets: vec![
                SecretDecl {
                    name: "GRAFANA_TOKEN".into(),
                    description: None,
                    reason: None,
                },
                SecretDecl {
                    name: "OTHER".into(),
                    description: None,
                    reason: None,
                },
            ],
            sandbox: None,
            description: None,
        };
        let specs = cfg.secret_specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id, "GRAFANA_TOKEN");
        assert!(matches!(
            specs[0].source,
            SecretSource::Env { ref name } if name == "GRAFANA_TOKEN"
        ));
        assert!(matches!(
            specs[0].inject,
            SecretInjection::Env { ref name } if name == "GRAFANA_TOKEN"
        ));
        assert_eq!(specs[0].identity_token(), "env:GRAFANA_TOKEN");
    }

    #[test]
    fn secret_specs_http_bearer_compiles_to_authorization_inject() {
        let cfg = McpServerConfig {
            transport: Some(McpServerTransportConfig::StreamableHttp(
                StreamableHttpConfig {
                    url: "https://example.com/mcp".into(),
                    headers: vec![],
                    auth: Some(HttpAuthConfig::Bearer {
                        token_ref: HttpSecretRef {
                            source: SecretSource::Env {
                                name: "TOKEN".into(),
                            },
                        },
                    }),
                    timeouts: Default::default(),
                    pooling: Default::default(),
                    network_policy: Default::default(),
                },
            )),
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            secrets: vec![],
            sandbox: None,
            description: None,
        };
        let specs = cfg.secret_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].id, "auth.bearer");
        assert!(matches!(
            specs[0].inject,
            SecretInjection::HttpAuthorizationBearer
        ));
        assert!(matches!(
            specs[0].source,
            SecretSource::Env { ref name } if name == "TOKEN"
        ));
    }

    #[test]
    fn secret_specs_http_header_and_basic_compile_correctly() {
        let header_cfg = McpServerConfig {
            transport: Some(McpServerTransportConfig::StreamableHttp(
                StreamableHttpConfig {
                    url: "https://example.com/mcp".into(),
                    headers: vec![],
                    auth: Some(HttpAuthConfig::Header {
                        header: "X-API-Key".into(),
                        value_ref: HttpSecretRef {
                            source: SecretSource::Env {
                                name: "API_KEY".into(),
                            },
                        },
                    }),
                    timeouts: Default::default(),
                    pooling: Default::default(),
                    network_policy: Default::default(),
                },
            )),
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            secrets: vec![],
            sandbox: None,
            description: None,
        };
        let s = header_cfg.secret_specs();
        assert_eq!(s[0].id, "auth.header.X-API-Key");
        assert!(matches!(
            s[0].inject,
            SecretInjection::HttpHeader { ref name } if name == "X-API-Key"
        ));

        let basic_cfg = McpServerConfig {
            transport: Some(McpServerTransportConfig::StreamableHttp(
                StreamableHttpConfig {
                    url: "https://example.com/mcp".into(),
                    headers: vec![],
                    auth: Some(HttpAuthConfig::Basic {
                        username: "grafana".into(),
                        password_ref: HttpSecretRef {
                            source: SecretSource::Env {
                                name: "PWD".into(),
                            },
                        },
                    }),
                    timeouts: Default::default(),
                    pooling: Default::default(),
                    network_policy: Default::default(),
                },
            )),
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            secrets: vec![],
            sandbox: None,
            description: None,
        };
        let s = basic_cfg.secret_specs();
        assert_eq!(s[0].id, "auth.basic.grafana");
        assert!(matches!(
            s[0].inject,
            SecretInjection::HttpBasicPassword { ref username } if username == "grafana"
        ));
    }

    #[test]
    fn identity_token_includes_version_when_present() {
        let spec = SecretSpec {
            id: "x".into(),
            source: SecretSource::Env { name: "T".into() },
            inject: SecretInjection::HttpAuthorizationBearer,
            version: Some("v3".into()),
        };
        assert_eq!(spec.identity_token(), "env:T@v3");
    }

    #[test]
    fn missing_mcp_servers_key_yields_empty_file() {
        let parsed = McpServersFile::parse("{}").expect("empty parse");
        assert!(parsed.servers.is_empty());
    }
}
