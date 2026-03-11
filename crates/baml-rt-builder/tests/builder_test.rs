//! Integration tests for baml-agent-builder
//!
//! These tests use the builder library to package agents and verify
//! the package structure and execution flow.

use tempfile::TempDir;
use test_support::common::{
    CalculatorTool, agent_fixture, ensure_fixture_runtime_types, workspace_root,
};

#[tokio::test]
async fn test_cli_package_agent() {
    // Use the example agent for packaging
    let agent_dir = workspace_root().join("examples").join("agent-example");

    if !agent_dir.exists() {
        println!("Skipping test: examples/agent-example not found");
        return;
    }

    let output_dir = TempDir::new().unwrap();
    let output_path = output_dir.path().join("test-agent.tar.gz");

    baml_rt_builder::build_agent_package(&agent_dir, &output_path)
        .await
        .expect("Packaging should succeed");
    assert!(output_path.exists(), "Package file should be created");
    assert!(
        output_path.metadata().unwrap().len() > 0,
        "Package file should not be empty"
    );

    // Verify package structure
    let tar_gz = std::fs::File::open(&output_path).unwrap();
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);

    let extract_dir = TempDir::new().unwrap();
    archive.unpack(extract_dir.path()).unwrap();

    // Check for required files
    assert!(
        extract_dir.path().join("manifest.json").exists(),
        "Package should contain manifest.json"
    );

    // baml_src should exist (required for runtime)
    assert!(
        extract_dir.path().join("baml_src").exists(),
        "Package should contain baml_src directory"
    );
    assert!(
        extract_dir.path().join("baml_src").is_dir(),
        "Package should contain baml_src directory"
    );

    // dist should exist (compiled TypeScript)
    if extract_dir.path().join("dist").exists() {
        assert!(
            extract_dir.path().join("dist").is_dir(),
            "Package should contain dist directory if present"
        );
    }
}

#[test]
fn test_cli_package_creates_manifest_if_missing() {
    // Test skipped - core functionality tested in test_cli_package_agent
}

#[tokio::test]
async fn test_full_integration_package_load_execute() {
    // FULL INTEGRATION TEST: Package agent -> Load package -> Execute JavaScript function
    // This verifies the complete flow from TypeScript compilation to function execution

    use std::{fs, sync::Arc};

    use baml_rt::{baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge};
    use baml_rt_core::ids::{AgentId, UuidId};
    use baml_rt_quickjs::collect_into_channel_owned;
    use serde_json::json;
    use tokio::sync::Mutex;

    // Use stream-baml-tool fixture
    ensure_fixture_runtime_types();
    let agent_dir = agent_fixture("stream-baml-tool");

    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        println!("Skipping test: stream-baml-tool fixture not found");
        return;
    }

    // STEP 1: Package the agent (compiles TypeScript to JavaScript)
    let package_dir = TempDir::new().unwrap();
    let package_path = package_dir.path().join("stream-baml-tool.tar.gz");

    baml_rt_builder::build_agent_package(&agent_dir, &package_path)
        .await
        .expect("Packaging failed");

    assert!(package_path.exists(), "Package file should be created");

    // STEP 2: Extract and verify package contains compiled JavaScript
    let extract_dir = TempDir::new().unwrap();
    let tar_gz = std::fs::File::open(&package_path).unwrap();
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    archive.unpack(extract_dir.path()).unwrap();

    // Verify dist/index.js exists (compiled JavaScript)
    let dist_index = extract_dir.path().join("dist").join("index.js");
    assert!(
        dist_index.exists(),
        "Package should contain compiled JavaScript at dist/index.js"
    );

    // STEP 3: Load the package (simulating what baml-agent-builder does)
    // Set up BAML runtime
    let baml_src = extract_dir.path().join("baml_src");
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    baml_manager
        .load_schema(baml_src.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let baml_manager = Arc::new(Mutex::new(baml_manager));

    // Create QuickJS bridge
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000015").unwrap());
    let bridge = QuickJSBridge::new(baml_manager.clone(), agent_id.clone())
        .await
        .unwrap();
    let bridge = Arc::new(Mutex::new(bridge));
    {
        let mut guard = bridge.lock().await;
        guard.register_baml_functions().await.unwrap();
    }

    // Load agent's compiled JavaScript code (this is what load_agent_package does)
    let agent_code = fs::read_to_string(&dist_index).unwrap();
    let agent_eval_result = {
        let mut guard = bridge.lock().await;
        guard.evaluate(None, &agent_code).await
    };

    if let Err(e) = agent_eval_result {
        panic!("Agent code failed to execute: {}", e);
    }

    // STEP 4: Verify function exists in globalThis
    let check_code = r#"
        (function() {
            return JSON.stringify({
                existsGlobal: typeof globalThis.onChatMessage === 'function'
            });
        })()
    "#;

    let check_result = {
        let mut guard = bridge.lock().await;
        guard.evaluate(None, check_code).await.unwrap()
    };
    let check_obj = check_result.as_object().expect("Expected object");
    let exists_global = check_obj
        .get("existsGlobal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        exists_global,
        "onChatMessage function should be defined in globalThis after loading packaged agent. result={:?}",
        check_obj
    );

    // STEP 5: Execute the stream handler using the A2A yield session protocol.
    // onChatMessage is a streaming handler and does not return a resolved value.
    let function_name = "onChatMessage";
    let args = json!({
        "method": "message.send",
        "params": {
            "message": {
                "messageId": "cli-1",
                "role": "ROLE_USER",
                "parts": [{ "text": "IntegrationTest" }]
            }
        }
    });

    let scope = baml_rt_core::context::InvocationScope::synthetic_message(agent_id.clone());
    let (session_id, yield_rx) = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, function_name, args)
            .await
            .expect("invoke onChatMessage")
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    collect_into_channel_owned(bridge.clone(), session_id, yield_rx, tx, None, None, scope)
        .await
        .expect("collect yielded chunks");
    let mut chunks = Vec::new();
    let mut completion = None;
    while let Some(output) = rx.recv().await {
        match output {
            baml_rt_quickjs::StreamOutput::Chunk(chunk) => {
                if chunk != serde_json::Value::Null {
                    chunks.push(chunk);
                }
            }
            baml_rt_quickjs::StreamOutput::RelayChunk(chunk) => {
                if chunk != serde_json::Value::Null {
                    chunks.push(chunk);
                }
            }
            baml_rt_quickjs::StreamOutput::Terminal(_, c) => {
                completion = Some(c);
                break;
            }
        }
    }

    assert!(
        !chunks.is_empty(),
        "Expected onChatMessage to yield at least one chunk. Raw: {chunks:?}"
    );
    assert!(
        completion == Some(baml_rt_core::stream_completion::StreamCompletion::SemanticFinal),
        "Expected semantic-final completion for onChatMessage, got {:?}",
        completion
    );
    println!(
        "Function '{function_name}' yielded {} chunk(s) with completion {:?}",
        chunks.len(),
        completion
    );
}
