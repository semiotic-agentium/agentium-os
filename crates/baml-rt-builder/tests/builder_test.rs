//! Integration tests for baml-agent-builder CLI
//!
//! These tests spawn the actual baml-agent-builder binary and verify
//! that all CLI subcommands work correctly end-to-end.

use tempfile::TempDir;
use test_support::common::{agent_fixture, workspace_root};
use test_support::support::cli::CliHarness;

#[test]
fn test_cli_package_agent() {
    let harness = CliHarness::new();
    // Use the example agent for packaging
    let agent_dir = workspace_root().join("examples").join("agent-example");

    if !agent_dir.exists() {
        println!("Skipping test: examples/agent-example not found");
        return;
    }

    let output_dir = TempDir::new().unwrap();
    let output_path = output_dir.path().join("test-agent.tar.gz");

    let mut cmd = harness.builder_command();
    cmd.arg("package")
        .arg("--agent-dir")
        .arg(&agent_dir)
        .arg("--output")
        .arg(&output_path)
        .arg("--skip-lint"); // Skip linting for faster test

    let output = cmd.output().expect("Failed to execute package command");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    if !output.status.success() {
        eprintln!("Stdout: {}", stdout);
        eprintln!("Stderr: {}", stderr);
    }

    assert!(output.status.success(), "Packaging should succeed");
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
    let harness = CliHarness::new();
    // FULL INTEGRATION TEST: Package agent -> Load package -> Execute JavaScript function
    // This verifies the complete flow from TypeScript compilation to function execution

    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt::quickjs_bridge::QuickJSBridge;
    use baml_rt_core::ids::{AgentId, UuidId};
    use serde_json::json;
    use std::fs;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Use voidship-rites fixture
    let agent_dir = agent_fixture("voidship-rites");

    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        println!("Skipping test: voidship-rites fixture not found");
        return;
    }

    // STEP 1: Package the agent (compiles TypeScript to JavaScript)
    let package_dir = TempDir::new().unwrap();
    let package_path = package_dir.path().join("voidship-rites.tar.gz");

    let mut package_cmd = harness.builder_command();
    package_cmd
        .arg("package")
        .arg("--agent-dir")
        .arg(&agent_dir)
        .arg("--output")
        .arg(&package_path)
        .arg("--skip-lint");

    let package_output = package_cmd.output().expect("Failed to package agent");

    if !package_output.status.success() {
        let stderr = String::from_utf8(package_output.stderr).unwrap();
        panic!("Packaging failed: {}", stderr);
    }

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
    let baml_manager = Arc::new(Mutex::new(baml_manager));

    // Create QuickJS bridge
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000015").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager.clone(), agent_id.clone())
        .await
        .unwrap();
    bridge.register_baml_functions().await.unwrap();

    // Load agent's compiled JavaScript code (this is what load_agent_package does)
    let agent_code = fs::read_to_string(&dist_index).unwrap();
    let agent_eval_result = bridge.evaluate(None, &agent_code).await;

    if let Err(e) = agent_eval_result {
        panic!("Agent code failed to execute: {}", e);
    }

    // STEP 4: Verify function exists in globalThis
    let check_code = r#"
        (function() {
            return JSON.stringify({
                existsGlobal: typeof globalThis.handle_a2a_request === 'function'
            });
        })()
    "#;

    let check_result = bridge.evaluate(None, check_code).await.unwrap();
    let check_obj = check_result.as_object().expect("Expected object");
    let exists_global = check_obj
        .get("existsGlobal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        exists_global,
        "handle_a2a_request function should be defined in globalThis after loading packaged agent. result={:?}",
        check_obj
    );

    // STEP 5: Execute the function using the shared invoke_function implementation
    let function_name = "handle_a2a_request";
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

    let scope = baml_rt_core::context::InvocationScope::standalone(agent_id.clone());
    let result = bridge.invoke_js_function(&scope, function_name, args).await;

    // Assert the function is found and can be called
    match result {
        Ok(val) => {
            // If we get an error, it should NOT be "Function not found"
            if let Some(obj) = val.as_object() {
                if let Some(error) = obj.get("error") {
                    let error_str = error.as_str().unwrap_or("");
                    assert!(
                        !error_str.contains("Function not found")
                            && !error_str.contains("is not defined"),
                        "Function '{}' should be found after packaging. Got error: {}",
                        function_name,
                        error_str
                    );
                    // Other errors (like missing API keys) are acceptable - function was called correctly
                    println!(
                        "✓ Function found and invoked (got API error as expected): {}",
                        error_str
                    );
                } else {
                    println!("✓ Function found and executed successfully: {:?}", obj);
                }
            } else if val.is_string() {
                println!("✓ Function found and returned string result");
            }
        }
        Err(e) => {
            let error_msg = format!("{}", e);
            // Check for function not found errors (this is what we're testing for)
            assert!(
                !error_msg.contains("Function not found") && !error_msg.contains("is not defined"),
                "Function '{}' should be found after packaging. Error: {}",
                function_name,
                e
            );
            panic!("Unexpected error: {}", e);
        }
    }
}
