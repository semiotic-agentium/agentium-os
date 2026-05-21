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
    sync::Arc,
    time::{Duration, Instant},
};

use baml_rt_mcp::runtime::{ConnectionError, McpCancelSlot, McpConnection};
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_cache::read_server,
    mcp_config::HttpHeader,
    mcp_snapshot::{Digest, McpApprovalState},
    tool_fsm::ToolStep,
    tools::ToolName,
};
use serde_json::json;

mod support;

use support::http_mcp::*;

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
        Some(Digest::new(
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )),
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
