//! End-to-end runtime tests for the rmcp Streamable HTTP transport against
//! an in-process axum fake server.
//!
//! The HTTPS variant uses axum-server + rustls with an rcgen self-signed cert
//! whose PEM is fed back to the platform transport through
//! `HttpLaunchConfig::extra_ca_certs_pem`. That field is the same one a
//! production deployment with a private CA would populate, so the test
//! exercises the real reqwest trust path rather than a
//! `danger_accept_invalid_certs` shortcut.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Once},
    time::{Duration, Instant},
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
    runtime::{
        ConnectionError, HttpLaunchConfig, LaunchKind, McpCancelSlot, McpConnection, ServerLaunch,
    },
};
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_cache::{read_server, write_snapshot},
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
    tool_fsm::{ToolSessionId, ToolStep},
    tools::{ToolAccess, ToolName, ToolSessionContext},
};
use rcgen::generate_simple_self_signed;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex};

const SERVER_NAME: &str = "remote-fixture";
const SERVER_ID: &str = "remote";
const TOOL_NAME: &str = "echo";
const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Default)]
struct ServerState {
    tool_schema: Value,
    call_result: Value,
    observed: Vec<(String, String, String)>,
    expire_next_call: bool,
    delay_next_call: bool,
    session_id: String,
}

type SharedState = Arc<Mutex<ServerState>>;

fn fixture_identity_digest() -> Digest {
    compute_server_identity_digest(
        &json!({ "tools": { "listChanged": true } }),
        &json!({ "name": SERVER_NAME }),
    )
}

fn schema() -> Value {
    json!({ "type": "object", "properties": { "q": { "type": "string" } } })
}

fn rotated_schema() -> Value {
    json!({ "type": "object", "properties": { "q": { "type": "integer" } } })
}

fn call_result_ok() -> Value {
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

fn make_state(call_result: Value, expire_next_call: bool) -> SharedState {
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
async fn spawn_http(state: SharedState) -> String {
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
async fn spawn_https(state: SharedState) -> (String, Vec<u8>) {
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

fn http_server_config(url: &str, headers: Vec<HttpHeader>) -> McpServerConfig {
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

fn approved_tool(schema: Value) -> McpImportedTool {
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

fn write_http_snapshot(
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

fn build_resolver(
    server_config: McpServerConfig,
    cache_root: &Path,
) -> McpResolver<EnvSecretResolver> {
    let mut servers = BTreeMap::new();
    servers.insert(SERVER_ID.into(), server_config);
    let file = McpServersFile { servers };
    McpResolver::new(cache_root.to_path_buf(), file, EnvSecretResolver)
}

fn session_context(name: &ToolName) -> ToolSessionContext {
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
fn https_launch_direct(
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
        server_config_digest: "sha256:direct-construction".into(),
        protocol_version: PROTOCOL_VERSION.into(),
        expected_identity_digest: identity.as_str().to_string(),
        expected_tools_digest: tools_digest.as_str().to_string(),
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

fn http_launch_direct(url: &str) -> ServerLaunch {
    let tools = vec![approved_tool(schema())];
    let identity = fixture_identity_digest();
    let tools_digest = compute_tools_digest(&tools);
    ServerLaunch {
        server_id: SERVER_ID.into(),
        startup_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(10),
        server_config_digest: "sha256:direct-construction".into(),
        protocol_version: PROTOCOL_VERSION.into(),
        expected_identity_digest: identity.as_str().to_string(),
        expected_tools_digest: tools_digest.as_str().to_string(),
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

fn bearer_secret(value: &str) -> ResolvedSecret {
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

fn basic_secret(username: &str, password: &str) -> ResolvedSecret {
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

fn header_secret(name: &str, value: &str) -> ResolvedSecret {
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

// -- plain-HTTP tests through the resolver/snapshot path -------------------

#[tokio::test]
async fn http_happy_path_init_list_call() {
    let state = make_state(call_result_ok(), false);
    let url = spawn_http(state.clone()).await;

    let cache = tempfile::tempdir().expect("tempdir");
    let server_config = http_server_config(&url, vec![]);
    write_http_snapshot(
        cache.path(),
        &server_config,
        vec![approved_tool(schema())],
        None,
    );

    let resolver = build_resolver(server_config, cache.path());
    let name = ToolName::parse(&format!("mcp/{SERVER_ID}/{TOOL_NAME}")).expect("tool name");
    let (metadata, handler) = resolver
        .resolve(&name)
        .expect("resolve ok")
        .expect("tool resolves");
    assert_eq!(metadata.name, name);

    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open session");
    session
        .send(json!({ "q": "cpu" }))
        .await
        .expect("send tools/call");
    let step = session.read(json!({})).await.expect("read response");
    match step {
        ToolStep::Done {
            output: Some(envelope),
        } => {
            assert_eq!(envelope["is_error"], false);
            let text = envelope["content"][0]["text"].as_str().expect("text block");
            assert_eq!(text, "echoed: cpu");
        }
        other => panic!("expected Done with envelope, got {other:?}"),
    }
    session.finish().await.expect("finish");

    let guard = state.lock().await;
    let methods: Vec<&str> = guard
        .observed
        .iter()
        .map(|(m, _, _)| m.as_str())
        .filter(|m| !m.is_empty())
        .collect();
    assert!(methods.contains(&"initialize"));
    assert!(methods.contains(&"tools/list"));
    assert!(methods.contains(&"tools/call"));
}

#[tokio::test]
async fn http_static_custom_header_reaches_server() {
    let state = make_state(call_result_ok(), false);
    let url = spawn_http(state.clone()).await;

    let cache = tempfile::tempdir().expect("tempdir");
    let server_config = http_server_config(
        &url,
        vec![HttpHeader {
            name: "X-Tenant".into(),
            value: "tenant-42".into(),
        }],
    );
    write_http_snapshot(
        cache.path(),
        &server_config,
        vec![approved_tool(schema())],
        None,
    );

    let resolver = build_resolver(server_config, cache.path());
    let name = ToolName::parse(&format!("mcp/{SERVER_ID}/{TOOL_NAME}")).expect("tool name");
    let (_, handler) = resolver
        .resolve(&name)
        .expect("resolve ok")
        .expect("tool resolves");

    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open session");
    session
        .send(json!({ "q": "cpu" }))
        .await
        .expect("send tools/call");
    let _ = session.read(json!({})).await.expect("read");
    session.finish().await.expect("finish");

    let guard = state.lock().await;
    let tenant_observed = guard.observed.iter().any(|(method, name, value)| {
        method == "tools/call" && name == "x-tenant" && value == "tenant-42"
    });
    assert!(
        tenant_observed,
        "X-Tenant: tenant-42 must reach the server on tools/call; observed={:?}",
        guard.observed
    );
}

#[tokio::test]
async fn http_server_config_digest_mismatch_fails_at_resolve() {
    let state = make_state(call_result_ok(), false);
    let url = spawn_http(state.clone()).await;

    let cache = tempfile::tempdir().expect("tempdir");
    let server_config = http_server_config(&url, vec![]);
    write_http_snapshot(
        cache.path(),
        &server_config,
        vec![approved_tool(schema())],
        Some(Digest::new("sha256:not-the-real-digest")),
    );

    let resolver = build_resolver(server_config, cache.path());
    let name = ToolName::parse(&format!("mcp/{SERVER_ID}/{TOOL_NAME}")).expect("tool name");
    let err = match resolver.resolve(&name) {
        Err(err) => err,
        Ok(_) => panic!("digest mismatch must fail closed at resolve"),
    };
    assert!(
        err.to_string().contains("launch config digest mismatch"),
        "unexpected error: {err}"
    );

    let guard = state.lock().await;
    assert!(
        guard.observed.is_empty(),
        "digest mismatch must not connect; observed={:?}",
        guard.observed
    );
}

#[tokio::test]
async fn http_session_expired_404_surfaces_error_without_hang() {
    let state = make_state(call_result_ok(), true);
    let (url, cert_pem) = spawn_https(state.clone()).await;

    let conn = McpConnection::new(https_launch_direct(&url, cert_pem, vec![], vec![]));
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        conn.call_tool(TOOL_NAME, json!({ "q": "cpu" })),
    )
    .await
    .expect("call_tool must not hang on session-expired 404");
    match outcome.expect_err("404 after initialize must surface as error") {
        ConnectionError::SessionExpired { server_id } => assert_eq!(server_id, SERVER_ID),
        other => panic!("expected typed SessionExpired, got {other:?}"),
    }

    assert!(conn.is_dead(), "expired rmcp service should be marked dead");
    match conn
        .call_tool(TOOL_NAME, json!({ "q": "cpu" }))
        .await
        .expect_err("same connection must not silently recover")
    {
        ConnectionError::SessionExpired { server_id } => assert_eq!(server_id, SERVER_ID),
        other => panic!("expected typed SessionExpired on reused connection, got {other:?}"),
    }
}

#[tokio::test]
async fn http_session_expired_rebuilds_lazily_on_next_resolve() {
    let state = make_state(call_result_ok(), true);
    let url = spawn_http(state.clone()).await;

    let cache = tempfile::tempdir().expect("tempdir");
    let server_config = http_server_config(&url, vec![]);
    write_http_snapshot(
        cache.path(),
        &server_config,
        vec![approved_tool(schema())],
        None,
    );

    let resolver = build_resolver(server_config, cache.path());
    let name = ToolName::parse(&format!("mcp/{SERVER_ID}/{TOOL_NAME}")).expect("tool name");
    let (_, handler) = resolver
        .resolve(&name)
        .expect("first resolve ok")
        .expect("tool resolves");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open first session");
    let err = session
        .send(json!({ "q": "cpu" }))
        .await
        .expect_err("first call should expire session");
    assert!(
        format!("{err:?}")
            .to_ascii_lowercase()
            .contains("session expired"),
        "expected session-expired transport error, got {err:?}"
    );

    let (_, handler) = resolver
        .resolve(&name)
        .expect("second resolve should recreate dead entry")
        .expect("tool resolves after recreate");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open second session");
    session
        .send(json!({ "q": "cpu" }))
        .await
        .expect("second call should use rebuilt connection");
    let step = session
        .read(json!({}))
        .await
        .expect("read rebuilt response");
    match step {
        ToolStep::Done {
            output: Some(envelope),
        } => assert_eq!(envelope["content"][0]["text"], "echoed: cpu"),
        other => panic!("expected Done with envelope, got {other:?}"),
    }

    let guard = state.lock().await;
    let initialize_count = guard
        .observed
        .iter()
        .filter(|(method, _, _)| method == "initialize")
        .count();
    assert!(
        initialize_count >= 2,
        "lazy rebuild should initialize a fresh service; observed={:?}",
        guard.observed
    );
}

// -- HTTPS tests through the rmcp transport (auth header observation) -----

#[tokio::test]
async fn http_session_expired_rebuild_fails_closed_on_tools_digest_mismatch() {
    let state = make_state(call_result_ok(), true);
    let url = spawn_http(state.clone()).await;

    let cache = tempfile::tempdir().expect("tempdir");
    let server_config = http_server_config(&url, vec![]);
    write_http_snapshot(
        cache.path(),
        &server_config,
        vec![approved_tool(schema())],
        None,
    );

    let resolver = build_resolver(server_config, cache.path());
    let name = ToolName::parse(&format!("mcp/{SERVER_ID}/{TOOL_NAME}")).expect("tool name");
    let (_, handler) = resolver
        .resolve(&name)
        .expect("first resolve ok")
        .expect("tool resolves");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open first session");
    session
        .send(json!({ "q": "cpu" }))
        .await
        .expect_err("first call should expire session");

    {
        let mut guard = state.lock().await;
        guard.tool_schema = rotated_schema();
    }

    let (_, handler) = resolver
        .resolve(&name)
        .expect("second resolve should recreate dead entry")
        .expect("tool resolves after recreate");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .expect("open second session");
    let err = session
        .send(json!({ "q": "cpu" }))
        .await
        .expect_err("startup digest mismatch should fail closed");
    assert!(
        format!("{err:?}").contains("tool surface digest mismatch"),
        "expected tools digest mismatch, got {err:?}"
    );

    let server = read_server(cache.path(), SERVER_ID).expect("server record after stale mark");
    assert_eq!(server.approval.state, McpApprovalState::Stale);
}

#[tokio::test]
async fn http_cancel_handle_terminates_local_call_without_waiting_for_server() {
    let state = make_state(call_result_ok(), false);
    {
        let mut guard = state.lock().await;
        guard.delay_next_call = true;
    }
    let url = spawn_http(state.clone()).await;
    let conn = Arc::new(McpConnection::new(http_launch_direct(&url)));
    let cancel_slot: McpCancelSlot = Default::default();

    let call = {
        let conn = conn.clone();
        let cancel_slot = cancel_slot.clone();
        tokio::spawn(async move {
            conn.call_tool_with_cancel_slot(TOOL_NAME, json!({ "q": "cpu" }), Some(cancel_slot))
                .await
        })
    };

    let cancel_handle = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(handle) = cancel_slot.lock().await.clone() {
                return handle;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancel handle should be registered before response await");
    cancel_handle.cancel_local();

    match call.await.expect("call task joins") {
        Err(ConnectionError::CallCancelled { server_id, .. }) => assert_eq!(server_id, SERVER_ID),
        other => panic!("expected local call cancellation, got {other:?}"),
    }

    assert!(
        cancel_slot.lock().await.is_none(),
        "cancel slot should clear after local cancellation"
    );
}

#[tokio::test]
async fn http_cancel_notify_is_bounded_while_call_is_in_flight() {
    let state = make_state(call_result_ok(), false);
    {
        let mut guard = state.lock().await;
        guard.delay_next_call = true;
    }
    let url = spawn_http(state.clone()).await;
    let conn = Arc::new(McpConnection::new(http_launch_direct(&url)));
    let cancel_slot: McpCancelSlot = Default::default();

    let call = {
        let conn = conn.clone();
        let cancel_slot = cancel_slot.clone();
        tokio::spawn(async move {
            conn.call_tool_with_cancel_slot(TOOL_NAME, json!({ "q": "cpu" }), Some(cancel_slot))
                .await
        })
    };

    let cancel_handle = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(handle) = cancel_slot.lock().await.clone() {
                return handle;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancel handle should be registered before response await");

    let started = Instant::now();
    let _ = cancel_handle.cancel(Some("client cancel".into())).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancel notification should be bounded and not wait for delayed tools/call"
    );

    match call.await.expect("call task joins") {
        Err(ConnectionError::CallCancelled { server_id, .. }) => assert_eq!(server_id, SERVER_ID),
        other => panic!("expected local call cancellation, got {other:?}"),
    }
}

#[tokio::test]
async fn https_bearer_authorization_reaches_server() {
    let state = make_state(call_result_ok(), false);
    let (url, cert_pem) = spawn_https(state.clone()).await;

    let launch = https_launch_direct(&url, cert_pem, vec![bearer_secret("tok-123")], vec![]);
    let conn = McpConnection::new(launch);
    let _ = conn
        .call_tool(TOOL_NAME, json!({ "q": "cpu" }))
        .await
        .expect("tools/call over https with bearer");

    let guard = state.lock().await;
    let saw = guard.observed.iter().any(|(method, name, value)| {
        method == "tools/call" && name == "authorization" && value == "Bearer tok-123"
    });
    assert!(
        saw,
        "expected `Authorization: Bearer tok-123` on tools/call; observed={:?}",
        guard.observed
    );
}

#[tokio::test]
async fn https_basic_authorization_reaches_server() {
    let state = make_state(call_result_ok(), false);
    let (url, cert_pem) = spawn_https(state.clone()).await;

    let launch = https_launch_direct(
        &url,
        cert_pem,
        vec![basic_secret("alice", "s3cret")],
        vec![],
    );
    let conn = McpConnection::new(launch);
    let _ = conn
        .call_tool(TOOL_NAME, json!({ "q": "cpu" }))
        .await
        .expect("tools/call over https with basic");

    // base64("alice:s3cret") == "YWxpY2U6czNjcmV0"
    let guard = state.lock().await;
    let saw = guard.observed.iter().any(|(method, name, value)| {
        method == "tools/call" && name == "authorization" && value == "Basic YWxpY2U6czNjcmV0"
    });
    assert!(
        saw,
        "expected `Authorization: Basic YWxpY2U6czNjcmV0` on tools/call; observed={:?}",
        guard.observed
    );
}

#[tokio::test]
async fn https_custom_header_secret_reaches_server() {
    let state = make_state(call_result_ok(), false);
    let (url, cert_pem) = spawn_https(state.clone()).await;

    let launch = https_launch_direct(
        &url,
        cert_pem,
        vec![header_secret("X-Tenant", "tenant-99")],
        vec![],
    );
    let conn = McpConnection::new(launch);
    let _ = conn
        .call_tool(TOOL_NAME, json!({ "q": "cpu" }))
        .await
        .expect("tools/call over https with header secret");

    let guard = state.lock().await;
    let saw = guard.observed.iter().any(|(method, name, value)| {
        method == "tools/call" && name == "x-tenant" && value == "tenant-99"
    });
    assert!(
        saw,
        "expected `X-Tenant: tenant-99` on tools/call; observed={:?}",
        guard.observed
    );
}
