// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_tools::{
    ExternalToolResolver, ManifestToolNames, ToolAccessPolicy, ToolName, ToolRegistry,
    approval::ApprovalState,
    external_tool_cache,
    external_tools::{
        ExternalRegistryResolver, ExternalToolDescribeSnapshot, ExternalToolManifest,
        ExternalToolMetadata, ExternalToolSnapshot, InvocationMode, MetadataSchemas,
        ProcessRuntimeSpec, SandboxImageRef, SandboxRuntimeSpec, ToolRuntime, ToolSchemaResult,
        compute_external_schema_digest,
        invoker::ExternalInvoker,
        now_snapshot_timestamp,
        resolver::{SandboxRuntimeWiring, SandboxSpecFactory},
        sandbox::{
            MockSandboxProvider, SandboxCache, SandboxCacheKey, SandboxProvider, SandboxSpec,
            SandboxSpecBuilder, ScriptedAdapter,
        },
        stdio::StdioSubprocessInvoker,
    },
    register_manifest_tools_with_fallback,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn external_tool_dev_mode_happy_path_registers_and_invokes() {
    let temp_root = unique_temp_dir("external-tools-e2e-happy");
    let temp_tool_dir = temp_root.join("tool");
    fs::create_dir_all(&temp_tool_dir).expect("create temp tool dir");

    let tool_server = temp_tool_dir.join("tool-server");
    let manifest = ExternalToolManifest {
        tool_abi_version: "1".to_string(),
        name: "support/e2e_echo".to_string(),
        description: "e2e echo".to_string(),
        bundle: "support".to_string(),
        local_name: "e2e_echo".to_string(),
        access_level: baml_rt_tools::tools::ToolAccess::Read,
        tags: vec![],
        event_sources: vec![],
        datasources: vec![],
        invocation_mode: baml_rt_tools::external_tools::InvocationMode::SingleShot,
        session_policy: Default::default(),
        secrets: vec![],
        secret_scope: Default::default(),
        capabilities: json!({}),
        config_bundle: None,
        runtime: Some(ToolRuntime::Process(
            baml_rt_tools::external_tools::ProcessRuntimeSpec {
                command: vec![tool_server.display().to_string()],
                setup: vec![],
            },
        )),
        coordination: None,
    };
    fs::write(
        temp_tool_dir.join("tool-manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    let schemas = MetadataSchemas {
        input: json!({"type": "object"}),
        output: json!({"type": "object"}),
        events: Vec::new(),
    };
    let schema_digest = compute_external_schema_digest(&manifest.clone().into_metadata(schemas));

    write_tool_server(
        &tool_server,
        &format!(
            "#!/bin/sh\n\
IFS= read -r req\n\
if printf '%s' \"$req\" | grep -q '\"method\":\"tool/describe\"'; then\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"support/e2e_echo\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\",\"tool/schema\"],\"schema_digest\":\"{schema_digest}\"}}}}'\n\
elif printf '%s' \"$req\" | grep -q '\"method\":\"tool/schema\"'; then\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"schema_version\":1,\"tool_name\":\"support/e2e_echo\",\"content_type\":\"application/schema+json\",\"content_digest\":\"{schema_digest}\",\"input\":{{\"type\":\"object\"}},\"output\":{{\"type\":\"object\"}}}}}}'\n\
else\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"output\":{{\"reply\":\"pong-from-external\"}},\"done\":true}}}}'\n\
fi\n"
        ),
    );

    let resolver = ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&temp_tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("resolver should load fixture tool");

    let registry = ToolRegistry::new();
    let manifest = ManifestToolNames::parse(&["support/e2e_echo".to_string()]).unwrap();
    register_manifest_tools_with_fallback(
        &registry,
        &manifest,
        &ToolAccessPolicy::permit_all(),
        Some(&resolver),
    )
    .expect("external manifest tool should register through fallback resolver");

    let context_id = ContextId::new(42, 7);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000777").unwrap());

    let session_id = registry
        .open_session("support/e2e_echo", json!({}), &context_id, &agent_id)
        .await
        .expect("open session");

    registry
        .session_send(&session_id, json!({ "message": "ping" }))
        .await
        .expect("send input");

    let step = registry
        .session_read(&session_id, Value::Null)
        .await
        .expect("read output");

    let output = match step {
        baml_rt_tools::ToolStep::Done { output } => output.expect("output payload"),
        other => panic!("expected Done step, got {other:?}"),
    };

    assert_eq!(
        output.get("reply").and_then(Value::as_str),
        Some("pong-from-external")
    );

    registry
        .session_finish(&session_id)
        .await
        .expect("finish session");

    let _ = fs::remove_dir_all(temp_root);
}

#[tokio::test]
async fn external_sandbox_session_tool_resolver_path_supports_suspend_resume() {
    let temp_root = unique_temp_dir("external-tools-e2e-session");
    let temp_tool_dir = temp_root.join("tool");
    let bind_root = temp_root.join("bind-rootfs");
    fs::create_dir_all(&temp_tool_dir).expect("create temp tool dir");
    fs::create_dir_all(&bind_root).expect("create bind root");

    let metadata = json!({
        "tool_abi_version": "1",
        "name": "support/session_echo",
        "description": "session e2e",
        "bundle": "support",
        "local_name": "session_echo",
        "access_level": "read",
        "invocation_mode": "session",
        "session_policy": "strict",
        "schemas": {
            "input": {"type": "object"},
            "output": {"type": "object"}
        },
        "secrets": [],
        "capabilities": {},
        "runtime": {
            "kind": "sandbox",
            "image": { "kind": "bind", "path": bind_root.display().to_string() },
            "adapter": {
                "schema_version": 1,
                "protocol": "jsonrpc-stdio",
                "command": ["python3", "/opt/tool/main.py"],
                "workdir": "/opt/tool"
            }
        }
    });

    let metadata: ExternalToolMetadata =
        serde_json::from_value(metadata).expect("metadata should parse");

    let adapter: baml_rt_tools::external_tools::sandbox::ScriptedAdapter = Arc::new(|stream| {
        tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(stream);
            let mut send_count = 0usize;
            let mut read_count = 0usize;
            loop {
                let mut len_buf = [0u8; 4];
                if r.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut body = vec![0u8; len];
                if r.read_exact(&mut body).await.is_err() {
                    break;
                }
                let req: Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let id = req.get("id").and_then(Value::as_u64).unwrap_or(1);
                let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                let params = req.get("params").cloned().unwrap_or(Value::Null);

                let response = match method {
                    "tool/session_open" => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"session_id": "sess-1"}
                    }),
                    "tool/session_send" => {
                        send_count += 1;
                        if send_count == 2 {
                            let resume = params
                                .get("resume_token")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if resume != "rt-1" {
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {"code": -32602, "message": "resume token mismatch"}
                                })
                            } else {
                                json!({"jsonrpc": "2.0", "id": id, "result": {}})
                            }
                        } else {
                            json!({"jsonrpc": "2.0", "id": id, "result": {}})
                        }
                    }
                    "tool/session_read" => {
                        read_count += 1;
                        if read_count == 1 {
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "step": "suspended",
                                    "output": {"need": "resume"},
                                    "resume_token": "rt-1"
                                }
                            })
                        } else {
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "step": "done",
                                    "output": {"reply": "resumed"}
                                }
                            })
                        }
                    }
                    "tool/session_finish" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    }),
                };

                let out = serde_json::to_vec(&response).expect("encode response");
                if w.write_all(&(out.len() as u32).to_be_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                if w.write_all(&out).await.is_err() {
                    break;
                }
                if w.flush().await.is_err() {
                    break;
                }
            }
        })
    });

    let provider: Arc<dyn SandboxProvider> = Arc::new(MockSandboxProvider::new(adapter));
    let cache = Arc::new(SandboxCache::new("runner-e2e"));
    let spec_factory: baml_rt_tools::external_tools::resolver::SandboxSpecFactory = {
        let cache = cache.clone();
        Arc::new(
            move |_tool_name: &baml_rt_tools::ToolName,
                  _meta: &baml_rt_tools::external_tools::ExternalToolMetadata| {
                let cache = cache.clone();
                let builder: baml_rt_tools::external_tools::sandbox::SandboxSpecBuilder = Arc::new(
                    move |key: &baml_rt_tools::external_tools::sandbox::SandboxCacheKey| {
                        Ok(SandboxSpec::for_test(
                            cache.encode_name(key),
                            "scratch:latest",
                        ))
                    },
                );
                Ok(builder)
            },
        )
    };

    let schema_digest = compute_external_schema_digest(&metadata);
    let mut snapshot = ExternalToolSnapshot::from_parts(
        &temp_tool_dir,
        ExternalToolManifest::from(metadata.clone()),
        ToolSchemaResult {
            schema_version: 1,
            tool_name: metadata.name.clone(),
            content_type: "application/schema+json".to_string(),
            content_digest: schema_digest.to_string(),
            input: metadata.schemas.input.clone(),
            output: metadata.schemas.output.clone(),
            events: Vec::new(),
        },
        ExternalToolDescribeSnapshot {
            protocol_version: "1".to_string(),
            supported_methods: baml_sandbox_protocol::SUPPORTED_METHODS_SESSION
                .iter()
                .map(|m| m.to_string())
                .collect(),
            max_payload_bytes: None,
            schema_digest: Some(schema_digest),
        },
        now_snapshot_timestamp(),
    )
    .expect("snapshot should build");
    snapshot.approval.state = ApprovalState::Approved;
    snapshot.approval.reviewed_at = Some(now_snapshot_timestamp());

    let resolver = ExternalRegistryResolver::from_snapshots(
        vec![snapshot],
        Some(SandboxRuntimeWiring {
            provider,
            cache,
            spec_factory,
        }),
        None,
    )
    .expect("resolver load should succeed");

    let registry = ToolRegistry::new();
    let manifest = ManifestToolNames::parse(&["support/session_echo".to_string()]).unwrap();
    register_manifest_tools_with_fallback(
        &registry,
        &manifest,
        &ToolAccessPolicy::permit_all(),
        Some(&resolver),
    )
    .expect("tool should register");

    let context_id = ContextId::new(50, 8);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000888").unwrap());

    let session_id = registry
        .open_session("support/session_echo", json!({}), &context_id, &agent_id)
        .await
        .expect("open session");

    registry
        .session_send(&session_id, json!({"message": "first"}))
        .await
        .expect("first send");

    let step_1 = registry
        .session_read(&session_id, Value::Null)
        .await
        .expect("first read");
    assert!(matches!(step_1, baml_rt_tools::ToolStep::Suspended { .. }));

    registry
        .session_send(&session_id, json!({"message": "resume"}))
        .await
        .expect("resume send with token should be injected");

    let step_2 = registry
        .session_read(&session_id, Value::Null)
        .await
        .expect("second read");

    let output = match step_2 {
        baml_rt_tools::ToolStep::Done { output } => output.expect("done output"),
        other => panic!("expected Done, got {other:?}"),
    };
    assert_eq!(output.get("reply").and_then(Value::as_str), Some("resumed"));

    registry
        .session_finish(&session_id)
        .await
        .expect("finish session");

    let _ = fs::remove_dir_all(temp_root);
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()))
}

fn write_tool_server(path: &Path, script: &str) {
    fs::write(path, script.as_bytes()).expect("write tool server script");
    let mut perms = fs::metadata(path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("set executable permissions");
}

// ---------------------------------------------------------------------------
// Phase 0: tool/schema invoker tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stdio_invoker_schema_returns_parsed_tool_schema_result() {
    let temp = unique_temp_dir("stdio-schema-happy");
    fs::create_dir_all(&temp).unwrap();
    let binary = temp.join("tool-server");

    write_tool_server(
        &binary,
        "#!/bin/sh\n\
        IFS= read -r req\n\
        if printf '%s' \"$req\" | grep -q '\"method\":\"tool/schema\"'; then\n\
          printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"schema_version\":1,\"tool_name\":\"support/weather\",\"content_type\":\"application/schema+json\",\"content_digest\":\"sha256:abc123\",\"input\":{\"type\":\"object\"},\"output\":{\"type\":\"object\"}}}'\n\
        fi\n",
    );

    let tool = baml_rt_tools::ToolName::parse("support/weather").unwrap();
    let invoker = StdioSubprocessInvoker::new(binary);
    let result = invoker
        .schema(&tool, std::time::Duration::from_secs(5))
        .await
        .expect("schema() should succeed");

    assert_eq!(result.tool_name, "support/weather");
    assert_eq!(result.content_type, "application/schema+json");
    assert_eq!(result.content_digest, "sha256:abc123");
    assert_eq!(result.input, json!({"type": "object"}));
    assert_eq!(result.output, json!({"type": "object"}));

    let _ = fs::remove_dir_all(temp);
}

#[tokio::test]
async fn stdio_invoker_schema_propagates_jsonrpc_error() {
    let temp = unique_temp_dir("stdio-schema-error");
    fs::create_dir_all(&temp).unwrap();
    let binary = temp.join("tool-server");

    // Tool returns METHOD_NOT_FOUND — simulates tool that doesn't support tool/schema.
    write_tool_server(
        &binary,
        "#!/bin/sh\n\
        IFS= read -r _req\n\
        printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"unknown method tool/schema\"}}'\n",
    );

    let tool = baml_rt_tools::ToolName::parse("support/weather").unwrap();
    let invoker = StdioSubprocessInvoker::new(binary);
    let err = invoker
        .schema(&tool, std::time::Duration::from_secs(5))
        .await
        .expect_err("schema() should fail when tool returns JSON-RPC error");

    let msg = err.to_string();
    assert!(
        msg.contains("unknown method tool/schema") || msg.contains("tool/schema"),
        "error should mention tool/schema, got: {msg}"
    );

    let _ = fs::remove_dir_all(temp);
}

#[tokio::test]
async fn stdio_invoker_schema_fails_on_malformed_result() {
    let temp = unique_temp_dir("stdio-schema-malformed");
    fs::create_dir_all(&temp).unwrap();
    let binary = temp.join("tool-server");

    // Tool returns a result that doesn't match ToolSchemaResult shape.
    write_tool_server(
        &binary,
        "#!/bin/sh\n\
        IFS= read -r _req\n\
        printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"unexpected_field\":true}}'\n",
    );

    let tool = baml_rt_tools::ToolName::parse("support/weather").unwrap();
    let invoker = StdioSubprocessInvoker::new(binary);
    let err = invoker
        .schema(&tool, std::time::Duration::from_secs(5))
        .await
        .expect_err("schema() should fail on malformed result");

    let msg = err.to_string();
    assert!(
        msg.contains("tool/schema"),
        "error should mention tool/schema, got: {msg}"
    );

    let _ = fs::remove_dir_all(temp);
}

// ---------------------------------------------------------------------------
// ExternalRegistryResolver::from_allowed_dirs discovery / reuse / drift
// ---------------------------------------------------------------------------

/// Count how many times the counting tool-server was spawned. Each `tool/describe`
/// and `tool/schema` RPC spawns the process once (stdio transport is one-shot),
/// so the byte length of the counter file is the total discovery call count.
fn spawn_count(counter: &Path) -> usize {
    fs::read(counter).map(|bytes| bytes.len()).unwrap_or(0)
}

fn process_manifest(tool_server: &Path, name: &str, description: &str) -> ExternalToolManifest {
    let (bundle, local) = name.split_once('/').expect("bundle/local tool name");
    ExternalToolManifest {
        tool_abi_version: "1".to_string(),
        name: name.to_string(),
        description: description.to_string(),
        bundle: bundle.to_string(),
        local_name: local.to_string(),
        access_level: baml_rt_tools::tools::ToolAccess::Read,
        tags: vec![],
        event_sources: vec![],
        datasources: vec![],
        invocation_mode: InvocationMode::SingleShot,
        session_policy: Default::default(),
        secrets: vec![],
        secret_scope: Default::default(),
        capabilities: json!({}),
        config_bundle: None,
        runtime: Some(ToolRuntime::Process(ProcessRuntimeSpec {
            command: vec![tool_server.display().to_string()],
            setup: vec![],
        })),
        coordination: None,
    }
}

fn write_manifest(dir: &Path, manifest: &ExternalToolManifest) {
    fs::write(
        dir.join("tool-manifest.json"),
        serde_json::to_vec_pretty(manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

/// Schema digest a tool's `tool/schema` response must self-report. Computed over
/// the `{input, output}` pair only, so it is stable across manifest description /
/// runtime edits (which only move the manifest/runtime digests).
fn schema_digest_for(manifest: &ExternalToolManifest) -> String {
    compute_external_schema_digest(&manifest.clone().into_metadata(MetadataSchemas {
        input: json!({"type": "object"}),
        output: json!({"type": "object"}),
        events: Vec::new(),
    }))
    .to_string()
}

/// A process tool-server that appends one byte to `counter` per spawn, then
/// answers `tool/describe` / `tool/schema` / `tool/invoke` over stdio JSON-RPC.
fn write_counting_tool_server(path: &Path, counter: &Path, name: &str, schema_digest: &str) {
    let counter = counter.display();
    let script = format!(
        "#!/bin/sh\n\
printf 'x' >> '{counter}'\n\
IFS= read -r req\n\
if printf '%s' \"$req\" | grep -q '\"method\":\"tool/describe\"'; then\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{name}\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\",\"tool/schema\"],\"schema_digest\":\"{schema_digest}\"}}}}'\n\
elif printf '%s' \"$req\" | grep -q '\"method\":\"tool/schema\"'; then\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"schema_version\":1,\"tool_name\":\"{name}\",\"content_type\":\"application/schema+json\",\"content_digest\":\"{schema_digest}\",\"input\":{{\"type\":\"object\"}},\"output\":{{\"type\":\"object\"}}}}}}'\n\
else\n\
  printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"output\":{{\"reply\":\"pong\"}},\"done\":true}}}}'\n\
fi\n"
    );
    write_tool_server(path, &script);
}

#[tokio::test]
async fn from_allowed_dirs_first_load_discovers_and_persists_snapshot() {
    let temp_root = unique_temp_dir("external-allowed-discover");
    let tool_dir = temp_root.join("tool");
    fs::create_dir_all(&tool_dir).expect("create tool dir");

    let tool_server = tool_dir.join("tool-server");
    let counter = temp_root.join("spawns.log");
    let name = "support/discover_echo";

    let manifest = process_manifest(&tool_server, name, "first");
    write_manifest(&tool_dir, &manifest);
    write_counting_tool_server(&tool_server, &counter, name, &schema_digest_for(&manifest));

    let snapshot_path = external_tool_cache::approved_snapshot_path(&temp_root, name).unwrap();
    assert!(!snapshot_path.exists(), "no snapshot before first load");

    let resolver = ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("first load should discover via tool/describe + tool/schema");

    assert_eq!(
        spawn_count(&counter),
        2,
        "first load must call tool/describe then tool/schema (two spawns)"
    );
    assert!(
        snapshot_path.is_file(),
        "approved snapshot persisted under <root>/external-tools/tools/<slug>/tool-snapshot.json"
    );

    let parsed = external_tool_cache::read_snapshot(&snapshot_path).unwrap();
    assert!(
        parsed.approval.state.is_approved(),
        "persisted snapshot is auto-approved"
    );
    assert_eq!(parsed.tool.name, name);

    let tool_name = ToolName::parse(name).unwrap();
    assert!(
        resolver.resolve(&tool_name).unwrap().is_some(),
        "resolver registers the discovered tool"
    );

    let _ = fs::remove_dir_all(temp_root);
}

#[tokio::test]
async fn from_allowed_dirs_second_load_reuses_snapshot_without_discovery() {
    let temp_root = unique_temp_dir("external-allowed-reuse");
    let tool_dir = temp_root.join("tool");
    fs::create_dir_all(&tool_dir).expect("create tool dir");

    let tool_server = tool_dir.join("tool-server");
    let counter = temp_root.join("spawns.log");
    let name = "support/reuse_echo";

    let manifest = process_manifest(&tool_server, name, "stable");
    write_manifest(&tool_dir, &manifest);
    write_counting_tool_server(&tool_server, &counter, name, &schema_digest_for(&manifest));

    let snapshot_path = external_tool_cache::approved_snapshot_path(&temp_root, name).unwrap();

    ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("first load discovers");
    assert_eq!(spawn_count(&counter), 2, "first load discovers");
    let after_first = fs::read_to_string(&snapshot_path).expect("snapshot after first load");

    // Same manifest + runtime: the approved snapshot's digests still match, so
    // the second load must reuse it and skip the tool entirely.
    let resolver = ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("second load reuses snapshot");

    assert_eq!(
        spawn_count(&counter),
        2,
        "reuse must NOT call tool/describe or tool/schema again"
    );
    let after_second = fs::read_to_string(&snapshot_path).expect("snapshot after second load");
    assert_eq!(
        after_first, after_second,
        "reused snapshot is byte-identical"
    );
    assert!(
        resolver
            .resolve(&ToolName::parse(name).unwrap())
            .unwrap()
            .is_some(),
        "reused tool still resolves"
    );

    let _ = fs::remove_dir_all(temp_root);
}

#[tokio::test]
async fn from_allowed_dirs_manifest_digest_change_triggers_rediscovery() {
    let temp_root = unique_temp_dir("external-allowed-drift");
    let tool_dir = temp_root.join("tool");
    fs::create_dir_all(&tool_dir).expect("create tool dir");

    let tool_server = tool_dir.join("tool-server");
    let counter = temp_root.join("spawns.log");
    let name = "support/drift_echo";

    let manifest = process_manifest(&tool_server, name, "first");
    write_manifest(&tool_dir, &manifest);
    // Schema digest is unchanged by a description edit, so one tool-server suffices.
    write_counting_tool_server(&tool_server, &counter, name, &schema_digest_for(&manifest));

    let snapshot_path = external_tool_cache::approved_snapshot_path(&temp_root, name).unwrap();

    ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("first load discovers");
    assert_eq!(spawn_count(&counter), 2, "first load discovers");
    let first = external_tool_cache::read_snapshot(&snapshot_path).unwrap();

    // Edit the manifest description: manifest_digest moves, schema_digest does not.
    let changed = process_manifest(&tool_server, name, "second");
    write_manifest(&tool_dir, &changed);

    let resolver = ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        None,
        None,
    )
    .await
    .expect("stale snapshot rediscovers");

    assert_eq!(
        spawn_count(&counter),
        4,
        "manifest digest drift must force a fresh tool/describe + tool/schema"
    );
    let second = external_tool_cache::read_snapshot(&snapshot_path).unwrap();
    assert_ne!(
        first.digests.manifest_digest, second.digests.manifest_digest,
        "rediscovered snapshot carries the new manifest digest"
    );
    assert_eq!(
        second.tool.description, "second",
        "approved snapshot overwritten with the edited manifest"
    );
    assert!(
        resolver
            .resolve(&ToolName::parse(name).unwrap())
            .unwrap()
            .is_some(),
        "rediscovered tool resolves"
    );

    let _ = fs::remove_dir_all(temp_root);
}

#[tokio::test]
async fn from_allowed_dirs_sandbox_bind_rootfs_discovers_with_mock_wiring() {
    let temp_root = unique_temp_dir("external-allowed-sandbox");
    let tool_dir = temp_root.join("tool");
    let bind_root = temp_root.join("bind-rootfs");
    fs::create_dir_all(&tool_dir).expect("create tool dir");
    fs::create_dir_all(&bind_root).expect("create bind root");

    let name = "support/sandbox_echo";
    let manifest = ExternalToolManifest {
        tool_abi_version: "1".to_string(),
        name: name.to_string(),
        description: "sandbox bind discovery".to_string(),
        bundle: "support".to_string(),
        local_name: "sandbox_echo".to_string(),
        access_level: baml_rt_tools::tools::ToolAccess::Read,
        tags: vec![],
        event_sources: vec![],
        datasources: vec![],
        invocation_mode: InvocationMode::SingleShot,
        session_policy: Default::default(),
        secrets: vec![],
        secret_scope: Default::default(),
        capabilities: json!({}),
        config_bundle: None,
        runtime: Some(ToolRuntime::Sandbox(SandboxRuntimeSpec {
            image: SandboxImageRef::Bind {
                path: bind_root.clone(),
            },
            entrypoint: vec!["/tool-adapter".to_string()],
            adapter: None,
        })),
        coordination: None,
    };
    write_manifest(&tool_dir, &manifest);
    let schema_digest = schema_digest_for(&manifest);

    // Scripted guest adapter answering discovery's tool/describe + tool/schema.
    let adapter_name = name.to_string();
    let adapter_digest = schema_digest.clone();
    let adapter: ScriptedAdapter = Arc::new(move |stream| {
        let name = adapter_name.clone();
        let digest = adapter_digest.clone();
        tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(stream);
            loop {
                let mut len_buf = [0u8; 4];
                if r.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut body = vec![0u8; len];
                if r.read_exact(&mut body).await.is_err() {
                    break;
                }
                let req: Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let id = req.get("id").and_then(Value::as_u64).unwrap_or(1);
                let method = req.get("method").and_then(Value::as_str).unwrap_or("");
                let response = match method {
                    "tool/describe" => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocol_version": "1",
                            "tool_name": name,
                            "supported_methods": ["tool/describe", "tool/invoke", "tool/schema"],
                            "schema_digest": digest,
                        }
                    }),
                    "tool/schema" => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "schema_version": 1,
                            "tool_name": name,
                            "content_type": "application/schema+json",
                            "content_digest": digest,
                            "input": {"type": "object"},
                            "output": {"type": "object"},
                        }
                    }),
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    }),
                };
                let out = serde_json::to_vec(&response).expect("encode response");
                if w.write_all(&(out.len() as u32).to_be_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                if w.write_all(&out).await.is_err() {
                    break;
                }
                if w.flush().await.is_err() {
                    break;
                }
            }
        })
    });

    let provider: Arc<dyn SandboxProvider> = Arc::new(MockSandboxProvider::new(adapter));
    let cache = Arc::new(SandboxCache::new("allowed-dirs-sandbox"));
    let spec_factory: SandboxSpecFactory = {
        let cache = cache.clone();
        Arc::new(move |_tool_name: &ToolName, _meta: &ExternalToolMetadata| {
            let cache = cache.clone();
            let builder: SandboxSpecBuilder = Arc::new(move |key: &SandboxCacheKey| {
                Ok(SandboxSpec::for_test(
                    cache.encode_name(key),
                    "scratch:latest",
                ))
            });
            Ok(builder)
        })
    };
    let wiring = SandboxRuntimeWiring {
        provider,
        cache,
        spec_factory,
    };

    let resolver = ExternalRegistryResolver::from_allowed_dirs(
        std::slice::from_ref(&tool_dir),
        &temp_root,
        Some(wiring),
        None,
    )
    .await
    .expect("sandbox bind-rootfs tool discovers through allowed dirs");

    let snapshot_path = external_tool_cache::approved_snapshot_path(&temp_root, name).unwrap();
    assert!(snapshot_path.is_file(), "sandbox snapshot persisted");

    let parsed = external_tool_cache::read_snapshot(&snapshot_path).unwrap();
    match parsed.tool.runtime {
        Some(ToolRuntime::Sandbox(ref spec)) => match &spec.image {
            SandboxImageRef::Bind { path } => {
                assert_eq!(path, &bind_root, "bind rootfs preserved through discovery")
            }
            other => panic!("expected bind image, got {other:?}"),
        },
        other => panic!("expected sandbox runtime, got {other:?}"),
    }

    assert!(
        resolver
            .resolve(&ToolName::parse(name).unwrap())
            .unwrap()
            .is_some(),
        "sandbox tool resolves with wiring"
    );

    let _ = fs::remove_dir_all(temp_root);
}
