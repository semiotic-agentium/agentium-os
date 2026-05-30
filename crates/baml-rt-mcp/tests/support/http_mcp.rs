// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Once},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use axum_server::tls_rustls::RustlsConfig;
use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_mcp::{
    importer::EnvSecretResolver,
    resolver::McpResolver,
    runtime::{HttpLaunchConfig, LaunchKind, ServerLaunch},
};
use baml_rt_tools::{
    mcp_cache::write_snapshot,
    mcp_config::{
        HttpHeader, HttpNetworkPolicyConfig, HttpPoolingConfig, HttpTimeoutsConfig,
        McpServerConfig, McpServerTransportConfig, McpServersFile, SecretInjection, SecretSource,
        SecretSpec, StreamableHttpConfig,
    },
    mcp_schema_normalize::normalize,
    mcp_secrets::ResolvedSecret,
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, compute_server_config_digest,
        compute_server_identity_digest, compute_tools_digest,
    },
    tool_fsm::ToolSessionId,
    tools::{ToolAccess, ToolName, ToolSessionContext},
};
use rcgen::generate_simple_self_signed;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex};

pub const SERVER_NAME: &str = "remote-fixture";
pub const SERVER_ID: &str = "remote";
pub const TOOL_NAME: &str = "echo";
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Default)]
pub struct ServerState {
    pub tool_schema: Value,
    pub call_result: Value,
    pub observed: Vec<(String, String, String)>,
    pub expire_next_call: bool,
    pub delay_next_call: bool,
    pub session_id: String,
}

pub type SharedState = Arc<Mutex<ServerState>>;

fn fixture_identity_digest() -> Digest {
    compute_server_identity_digest(
        &json!({ "tools": { "listChanged": true } }),
        &json!({ "name": SERVER_NAME }),
    )
}

pub fn schema() -> Value {
    json!({ "type": "object", "properties": { "q": { "type": "string" } } })
}

pub fn rotated_schema() -> Value {
    json!({ "type": "object", "properties": { "q": { "type": "integer" } } })
}

pub fn call_result_ok() -> Value {
    json!({
        "content": [{ "type": "text", "text": "echoed: cpu" }],
        "isError": false,
    })
}

fn record_headers(state: &mut ServerState, method: &str, headers: &HeaderMap) {
    for (k, v) in headers.iter() {
        if let Ok(value) = v.to_str() {
            state.observed.push((
                method.to_string(),
                k.as_str().to_string(),
                value.to_string(),
            ));
        }
    }
}

async fn post_mcp(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(msg): Json<Value>,
) -> Response {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let id = msg.get("id").cloned();
    let session_attached = headers.contains_key("mcp-session-id");

    let mut guard = state.lock().await;
    record_headers(&mut guard, &method, &headers);

    match method.as_str() {
        "initialize" => {
            if guard.session_id.is_empty() {
                guard.session_id = "sess-1".into();
            }
            let sid = guard.session_id.clone();
            let body = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": SERVER_NAME, "version": "0.1.0" },
                },
            });
            let mut hm = HeaderMap::new();
            hm.insert(
                "mcp-session-id",
                HeaderValue::from_str(&sid).expect("valid session header"),
            );
            (StatusCode::OK, hm, Json(body)).into_response()
        }
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let body = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": TOOL_NAME,
                        "inputSchema": guard.tool_schema.clone(),
                    }]
                },
            });
            Json(body).into_response()
        }
        "tools/call" => {
            if guard.expire_next_call && session_attached {
                guard.expire_next_call = false;
                return (StatusCode::NOT_FOUND, "session expired").into_response();
            }
            let delay = guard.delay_next_call;
            guard.delay_next_call = false;
            let result = guard.call_result.clone();
            drop(guard);
            if delay {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            let body = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            });
            Json(body).into_response()
        }
        "notifications/cancelled" => StatusCode::ACCEPTED.into_response(),
        other => {
            let body = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") },
            });
            Json(body).into_response()
        }
    }
}

async fn get_mcp() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn delete_mcp() -> Response {
    StatusCode::OK.into_response()
}

pub fn make_state(call_result: Value, expire_next_call: bool) -> SharedState {
    Arc::new(Mutex::new(ServerState {
        tool_schema: schema(),
        call_result,
        observed: vec![],
        expire_next_call,
        delay_next_call: false,
        session_id: String::new(),
    }))
}

/// Plain HTTP fake server; used by tests that exercise the resolver/digest
/// path without auth secrets.
pub async fn spawn_http(state: SharedState) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let app = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/mcp")
}

/// Install rustls' aws-lc-rs default crypto provider exactly once per test
/// process. Both axum-server's `tls-rustls` feature and reqwest's `rustls`
/// feature pull aws-lc-rs, but neither installs it as the process default —
/// without this, `RustlsConfig::from_pem` panics with `"no process-level
/// CryptoProvider available"`.
fn ensure_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Self-signed TLS server bound to `127.0.0.1`. Returns the connect URL plus
/// the cert PEM the client side needs to trust.
pub async fn spawn_https(state: SharedState) -> (String, Vec<u8>) {
    ensure_crypto_provider();
    let issued =
        generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("rcgen self-signed");
    let cert_pem = issued.cert.pem().into_bytes();
    let key_pem = issued.key_pair.serialize_pem().into_bytes();
    let rustls_config = RustlsConfig::from_pem(cert_pem.clone(), key_pem)
        .await
        .expect("rustls config");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");

    let app = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .with_state(state);

    tokio::spawn(async move {
        let _ = axum_server::from_tcp_rustls(listener, rustls_config)
            .serve(app.into_make_service())
            .await;
    });
    (format!("https://{addr}/mcp"), cert_pem)
}

pub fn http_server_config(url: &str, headers: Vec<HttpHeader>) -> McpServerConfig {
    McpServerConfig {
        transport: Some(McpServerTransportConfig::StreamableHttp(
            StreamableHttpConfig {
                url: url.into(),
                headers,
                auth: None,
                timeouts: HttpTimeoutsConfig::default(),
                pooling: HttpPoolingConfig::default(),
                network_policy: HttpNetworkPolicyConfig {
                    allow_hosts: vec![],
                    allow_private_ips: true,
                    follow_redirects: false,
                },
            },
        )),
        command: String::new(),
        args: vec![],
        env: BTreeMap::new(),
        secrets: vec![],
        sandbox: None,
        description: None,
    }
}

pub fn approved_tool(schema: Value) -> McpImportedTool {
    let input_schema_digest = normalize(&schema).digest;
    McpImportedTool {
        platform_tool_name: format!("mcp/{SERVER_ID}/{TOOL_NAME}"),
        mcp_tool_name: TOOL_NAME.into(),
        description: Some("echo description".into()),
        input_schema: schema,
        input_schema_digest,
        output_mode: McpOutputMode::ContentEnvelope,
        access_level: ToolAccess::Read,
        approval: ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("op@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        },
        opaque_fallback_reason: None,
        annotations: Value::Null,
    }
}

pub fn write_http_snapshot(
    cache_root: &Path,
    server_config: &McpServerConfig,
    tools: Vec<McpImportedTool>,
    override_server_config_digest: Option<Digest>,
) {
    let tools_digest = compute_tools_digest(&tools);
    let server_config_digest = override_server_config_digest.unwrap_or_else(|| {
        compute_server_config_digest(
            SERVER_ID,
            PROTOCOL_VERSION,
            server_config,
            Some(&tools_digest),
        )
    });
    let http_cfg = match &server_config.transport {
        Some(McpServerTransportConfig::StreamableHttp(http)) => http.clone(),
        _ => panic!("http snapshot helper requires StreamableHttp transport"),
    };
    let snapshot = McpServerSnapshot {
        schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
        server_id: SERVER_ID.into(),
        transport: McpTransportRef::StreamableHttp(http_cfg),
        protocol_version: PROTOCOL_VERSION.into(),
        server_info: None,
        server_config_digest,
        server_identity_digest: fixture_identity_digest(),
        tools_digest,
        secret_refs: vec![],
        approval: ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("op@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        },
        sandbox_profile: None,
        tools,
    };
    write_snapshot(cache_root, &snapshot).expect("persist snapshot");
}

pub fn build_resolver(
    server_config: McpServerConfig,
    cache_root: &Path,
) -> McpResolver<EnvSecretResolver> {
    let mut servers = BTreeMap::new();
    servers.insert(SERVER_ID.into(), server_config);
    let file = McpServersFile { servers };
    McpResolver::new(cache_root.to_path_buf(), file, EnvSecretResolver)
}

pub fn session_context(name: &ToolName) -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: name.clone(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(
            UuidId::parse_str("00000000-0000-0000-0000-000000000001").expect("valid uuid"),
        ),
        config: None,
        config_version: None,
        task_id: None,
        execution_classifier: None,
    }
}

/// Direct-construction launch for the TLS auth-observation tests. Bypasses
/// the snapshot/digest layer (already covered by the plain-HTTP tests) so the
/// CA cert PEM can be threaded in via `extra_ca_certs_pem`.
pub fn https_launch_direct(
    url: &str,
    cert_pem: Vec<u8>,
    resolved_secrets: Vec<ResolvedSecret>,
    static_headers: Vec<HttpHeader>,
) -> ServerLaunch {
    let tools = vec![approved_tool(schema())];
    let identity = fixture_identity_digest();
    let tools_digest = compute_tools_digest(&tools);
    ServerLaunch {
        server_id: SERVER_ID.into(),
        startup_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
        server_config_digest: Digest::new(
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        ),
        protocol_version: PROTOCOL_VERSION.into(),
        expected_identity_digest: identity,
        expected_tools_digest: tools_digest,
        cache_root: PathBuf::new(),
        kind: LaunchKind::Http(HttpLaunchConfig {
            url: url.into(),
            static_headers,
            resolved_secrets,
            network_policy: HttpNetworkPolicyConfig {
                allow_hosts: vec![],
                allow_private_ips: true,
                follow_redirects: false,
            },
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            idle_stream_timeout: Duration::from_secs(30),
            max_idle_per_host: 4,
            extra_ca_certs_pem: vec![cert_pem],
        }),
    }
}

pub fn http_launch_direct(url: &str) -> ServerLaunch {
    let tools = vec![approved_tool(schema())];
    let identity = fixture_identity_digest();
    let tools_digest = compute_tools_digest(&tools);
    ServerLaunch {
        server_id: SERVER_ID.into(),
        startup_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
        server_config_digest: Digest::new(
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        ),
        protocol_version: PROTOCOL_VERSION.into(),
        expected_identity_digest: identity,
        expected_tools_digest: tools_digest,
        cache_root: PathBuf::new(),
        kind: LaunchKind::Http(HttpLaunchConfig {
            url: url.into(),
            static_headers: vec![],
            resolved_secrets: vec![],
            network_policy: HttpNetworkPolicyConfig {
                allow_hosts: vec![],
                allow_private_ips: true,
                follow_redirects: false,
            },
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            idle_stream_timeout: Duration::from_secs(30),
            max_idle_per_host: 4,
            extra_ca_certs_pem: vec![],
        }),
    }
}

pub fn bearer_secret(value: &str) -> ResolvedSecret {
    ResolvedSecret {
        spec: SecretSpec {
            id: "auth.bearer".into(),
            source: SecretSource::Env { name: "TOK".into() },
            inject: SecretInjection::HttpAuthorizationBearer,
            version: None,
        },
        value: value.into(),
    }
}

pub fn basic_secret(username: &str, password: &str) -> ResolvedSecret {
    ResolvedSecret {
        spec: SecretSpec {
            id: format!("auth.basic.{username}"),
            source: SecretSource::Env { name: "PWD".into() },
            inject: SecretInjection::HttpBasicPassword {
                username: username.into(),
            },
            version: None,
        },
        value: password.into(),
    }
}

pub fn header_secret(name: &str, value: &str) -> ResolvedSecret {
    ResolvedSecret {
        spec: SecretSpec {
            id: format!("auth.header.{name}"),
            source: SecretSource::Env { name: "HDR".into() },
            inject: SecretInjection::HttpHeader { name: name.into() },
            version: None,
        },
        value: value.into(),
    }
}
