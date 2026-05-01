use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_tools::{
    ManifestToolNames, ToolAccessPolicy, ToolRegistry,
    external_tools::{
        DevModeResolver,
        resolver::SandboxRuntimeWiring,
        sandbox::{MockSandboxProvider, SandboxCache, SandboxProvider, SandboxSpec},
    },
    register_manifest_tools_with_fallback,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn external_tool_dev_mode_happy_path_registers_and_invokes() {
    let fixture_dir = fixture_tool_dir("happy");
    let temp_root = unique_temp_dir("external-tools-e2e-happy");
    let temp_tool_dir = temp_root.join("tool");
    fs::create_dir_all(&temp_tool_dir).expect("create temp tool dir");

    fs::copy(
        fixture_dir.join("tool-metadata.json"),
        temp_tool_dir.join("tool-metadata.json"),
    )
    .expect("copy metadata fixture");

    write_tool_server(
        &temp_tool_dir.join("tool-server"),
        "#!/bin/sh\n\
IFS= read -r req\n\
if printf '%s' \"$req\" | grep -q '\"method\":\"tool/describe\"'; then\n\
  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocol_version\":\"1\",\"tool_name\":\"support/e2e_echo\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"]}}'\n\
else\n\
  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"output\":{\"reply\":\"pong-from-external\"},\"done\":true}}'\n\
fi\n",
    );

    let resolver = DevModeResolver::from_dirs(std::slice::from_ref(&temp_tool_dir))
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
        },
        "runtime_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111"
    });

    fs::write(
        temp_tool_dir.join("tool-metadata.json"),
        serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
    )
    .expect("write metadata");

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

    let resolver = DevModeResolver::from_dirs_with_sandbox(
        std::slice::from_ref(&temp_tool_dir),
        None,
        baml_rt_tools::external_tools::ExternalLockfileMode::Off,
        None,
        SandboxRuntimeWiring {
            provider,
            cache,
            spec_factory,
        },
    )
    .await
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

fn fixture_tool_dir(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("external-tools")
        .join(case)
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
