use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_tools::{
    ManifestToolNames, ToolAccessPolicy, ToolRegistry, register_manifest_tools_with_fallback,
};
use baml_rt_tools::external_tools::DevModeResolver;
use serde_json::{Value, json};

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
  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocol_version\":\"1\",\"tool_name\":\"support/e2e_echo\",\"supported_methods\":[\"tool/invoke\"]}}'\n\
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

    assert_eq!(output.get("reply").and_then(Value::as_str), Some("pong-from-external"));

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
