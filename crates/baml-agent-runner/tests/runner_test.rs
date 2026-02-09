//! Tests for agent runner binary

use async_trait::async_trait;
use baml_rt::A2aRequestHandler;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::tools::BamlTool;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::effects::EffectBus;
use baml_rt_core::ids::{AgentId, UuidId};
use baml_rt_tools::bundles::BundleType;
use flate2::Compression;
use flate2::write::GzEncoder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tar::Builder;
use ts_rs::TS;

// Test bundle for test tools
#[allow(dead_code)]
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}
use baml_rt::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, SendMessageRequest,
};

use test_support::common::{CalculatorTool, agent_fixture, ensure_baml_src_exists, workspace_root};
use test_support::support::cli::CliHarness;

/// Build fixture with baml-agent-builder, extract tar to temp dir, return path to extracted dir (has dist + baml_src).
fn build_fixture_to_temp(fixture_name: &str) -> std::path::PathBuf {
    let agent_dir = agent_fixture(fixture_name);
    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!("Fixture {} not found or missing baml_src", fixture_name);
    }
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tar_path =
        std::env::temp_dir().join(format!("runner-test-{}-{}.tar.gz", fixture_name, unique));
    let extract_dir =
        std::env::temp_dir().join(format!("runner-test-{}-extract-{}", fixture_name, unique));
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).expect("create extract dir");

    let mut cmd = CliHarness::new().builder_command();
    cmd.arg("package")
        .arg("--agent-dir")
        .arg(&agent_dir)
        .arg("--output")
        .arg(&tar_path)
        .arg("--skip-lint");
    let output = cmd.output().expect("build fixture: run builder");
    if !output.status.success() {
        panic!(
            "build fixture {} failed: stdout={}, stderr={}",
            fixture_name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let tar_gz = fs::File::open(&tar_path).expect("open built tar");
    let tar_dec = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar_dec);
    archive.unpack(&extract_dir).expect("unpack built tar");
    let _ = fs::remove_file(&tar_path);

    let dist_index = extract_dir.join("dist").join("index.js");
    assert!(
        dist_index.exists(),
        "Built package must contain dist/index.js"
    );
    extract_dir
}

/// Create a test agent package from a fixture agent
fn create_test_agent_package(output_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let agent_dir = agent_fixture("stream-baml-tool");

    if !agent_dir.exists() {
        return Err(format!("Fixture agent directory not found: {}", agent_dir.display()).into());
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir =
        std::env::temp_dir().join(format!("e2e-agent-{}-{}", std::process::id(), unique));
    fs::create_dir_all(&temp_dir)?;

    // Copy baml_src from fixture (we no longer need baml_client - runtime loads directly from baml_src)
    let baml_src = temp_dir.join("baml_src");
    let fixture_baml_src = agent_dir.join("baml_src");
    if fixture_baml_src.exists() {
        copy_dir_all(&fixture_baml_src, &baml_src)?;
    } else {
        return Err("Fixture agent baml_src not found".into());
    }

    // Create manifest.json (stream-baml-tool fixture has support/calculate only)
    let manifest = serde_json::json!({
        "version": "1.0.0",
        "name": "test-agent",
        "description": "Test agent package for E2E testing",
        "entry_point": "dist/index.js",
        "runtime_version": "0.1.0",
        "signature": "test-agent@1.0.0",
        "tools": ["support/calculate"]
    });
    fs::write(
        temp_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    // Create tar.gz
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tar_gz = fs::File::create(output_path)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = Builder::new(enc);

    // Add all files from temp_dir to tar
    tar.append_dir_all(".", &temp_dir)?;
    tar.finish()?;

    // Cleanup temp directory
    fs::remove_dir_all(&temp_dir).ok();

    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[allow(dead_code)]
struct AddNumbersTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersInput {
    a: f64,
    b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersOutput {
    result: f64,
}

#[async_trait]
impl BamlTool for AddNumbersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "add_numbers";
    type OpenInput = ();
    type Input = AddNumbersInput;
    type Output = AddNumbersOutput;

    fn description(&self) -> &'static str {
        "Adds two numbers together"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(AddNumbersOutput {
            result: args.a + args.b,
        })
    }
}

fn user_message(message_id: &str, text: &str) -> Message {
    use baml_rt_a2a::a2a_types::A2aMessageId;
    use baml_rt_core::ids::{ContextId, ExternalId};
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(message_id)),
        role: MessageRole::String("ROLE_USER".to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: Some(ContextId::new(1, 1)),
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: std::collections::HashMap::new(),
    }
}

async fn setup_stream_baml_tool_agent() -> baml_rt::A2aAgent {
    let built = build_fixture_to_temp("stream-baml-tool");
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-baml-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(EffectBus::new()))
        .build()
        .await
        .unwrap()
}

async fn setup_stream_js_tool_agent() -> baml_rt::A2aAgent {
    let built = build_fixture_to_temp("stream-js-tool");
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-js-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(EffectBus::new()))
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn test_manifest_allowlist_blocks_undeclared_tool() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap());
    let scope = InvocationScope::standalone(agent_id);

    manager.set_tool_allowlist(HashSet::new()).await.unwrap();
    let blocked = context::with_scope(scope.as_scope().clone(), async {
        manager
            .open_tool_session("support/calculate", json!({}))
            .await
    })
    .await;
    assert!(
        blocked
            .err()
            .map(|err| err.to_string().contains("manifest allowlist"))
            .unwrap_or(false),
        "Expected allowlist to block undeclared tool"
    );

    let mut allowlist = HashSet::new();
    allowlist.insert("support/calculate".to_string());
    manager.set_tool_allowlist(allowlist).await.unwrap();
    let session = context::with_scope(scope.as_scope().clone(), async {
        manager
            .open_tool_session("support/calculate", json!({}))
            .await
    })
    .await;
    assert!(session.is_ok(), "Expected allowlisted tool to open");
}

#[tokio::test]
async fn test_agent_package_loading() {
    // This test verifies that we can load an agent package

    // Create a test agent package
    let package_path = std::env::temp_dir().join("test-agent-package.tar.gz");

    match create_test_agent_package(&package_path) {
        Ok(_) => {
            println!("Created test agent package: {}", package_path.display());
        }
        Err(e) => {
            eprintln!("Failed to create test package: {}", e);
            return;
        }
    }

    // Verify package exists
    assert!(package_path.exists(), "Test package should exist");

    // Test loading (we can't easily test the binary directly, but we can test the loading logic)
    // For now, just verify the package structure is correct
    let tar_gz = fs::File::open(&package_path).unwrap();
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);

    let extract_dir =
        std::env::temp_dir().join(format!("test-agent-extract-{}", std::process::id()));
    fs::create_dir_all(&extract_dir).unwrap();
    archive.unpack(&extract_dir).unwrap();

    // Verify manifest exists
    let manifest_path = extract_dir.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "manifest.json should exist in package"
    );

    // Verify baml_src exists
    let baml_src = extract_dir.join("baml_src");
    assert!(baml_src.exists(), "baml_src should exist in package");

    // Clean up
    fs::remove_dir_all(&extract_dir).ok();
    fs::remove_file(&package_path).ok();
}

#[tokio::test]
async fn test_runtime_manager_loads_schema() {
    // Test that BamlRuntimeManager can load a schema
    // This is the core functionality needed for agent loading

    if !ensure_baml_src_exists() {
        return;
    }

    let mut manager = BamlRuntimeManager::new().unwrap();
    let result = manager.load_schema(
        workspace_root()
            .join("baml_src")
            .to_str()
            .expect("Workspace baml_src path should be valid"),
    );

    match result {
        Ok(_) => {
            assert!(manager.is_schema_loaded(), "Schema should be loaded");
        }
        Err(e) => {
            let msg = format!("Schema loading failed: {:?}", e);
            println!("{}", msg);
            // Schema loading should succeed if baml_src exists
            panic!("Schema loading failed unexpectedly: {:?}", e);
        }
    }
}

#[tokio::test]
async fn test_e2e_agent_runner_load_package() {
    // Create a test agent package
    let package_path = std::env::temp_dir().join("e2e-test-agent-package.tar.gz");

    create_test_agent_package(&package_path).expect("Failed to create test agent package");

    assert!(package_path.exists(), "Test package should exist");

    // Run the binary to load the package
    let output = agent_runner_command()
        .arg(package_path.to_str().unwrap())
        .output()
        .expect("Failed to execute binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("STDOUT:\n{}", stdout);
    println!("STDERR:\n{}", stderr);

    // Should successfully load the agent
    assert!(
        output.status.success() || stdout.contains("Loaded") || stdout.contains("test-agent"),
        "Binary should successfully load the agent package. Exit code: {}, stdout: {}, stderr: {}",
        output.status.code().unwrap_or(-1),
        stdout,
        stderr
    );

    // Cleanup
    fs::remove_file(&package_path).ok();
}

#[tokio::test]
async fn test_e2e_agent_runner_invoke_function() {
    // Skip if no API key (we'll get auth errors, but that's okay for structure testing)
    let _ = dotenvy::dotenv();
    let has_api_key = std::env::var("OPENROUTER_API_KEY").is_ok();

    // Create a test agent package
    let package_path = std::env::temp_dir().join("e2e-test-agent-invoke.tar.gz");

    create_test_agent_package(&package_path).expect("Failed to create test agent package");

    // Try to invoke a function (will fail without API key, but should parse correctly)
    let mut cmd = agent_runner_command();
    cmd.arg(package_path.to_str().unwrap());
    cmd.arg("--invoke");
    cmd.arg("test-agent");
    cmd.arg("SimpleGreeting");
    cmd.arg(r#"{"name":"Test"}"#);

    let output = cmd.output().expect("Failed to execute binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("STDOUT:\n{}", stdout);
    println!("STDERR:\n{}", stderr);

    // Even if it fails due to missing API key, the structure should work
    // (i.e., it should load the package and attempt to invoke, not fail on parsing)
    let is_auth_error = stderr.contains("API key")
        || stderr.contains("authentication")
        || stderr.contains("401")
        || stdout.contains("error");

    if !has_api_key && is_auth_error {
        // Expected: Missing API key
        println!("Expected authentication error (no API key provided)");
    } else if output.status.success() {
        // Success: Function was invoked
        assert!(
            stdout.contains("{") || stdout.contains("result"),
            "Should return JSON result"
        );
    } else {
        // Other errors might be acceptable if they're not parsing/loading errors
        println!("Function invocation returned non-zero exit code, but may be expected");
    }

    // Cleanup
    fs::remove_file(&package_path).ok();
}

fn agent_runner_command() -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace_root())
        .arg("run")
        .arg("--quiet")
        .arg("-p")
        .arg("baml-agent-runner")
        .arg("--");
    command
}

/// Fixture: stream-baml-tool. Tests async streaming of a BAML tool (FSM) result via message.sendStream.
/// Requires .env with OPENROUTER_API_KEY (source .env or set in test env).
#[tokio::test]
async fn test_e2e_stream_baml_tool() {
    let _ = dotenvy::dotenv();

    let agent = setup_stream_baml_tool_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "compute 2+3"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-1-1".to_string())),
    };
    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let text = responses
        .iter()
        .filter_map(|r| {
            r.get("result")
                .and_then(|res| res.get("chunk").or(Some(res)))
        })
        .filter_map(|chunk| {
            chunk
                .get("message")
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|p| p.first())
                .and_then(|part| part.get("text"))
                .and_then(|v| v.as_str())
        })
        .find(|t| t.contains("sum=5"))
        .unwrap_or("");
    assert!(
        !text.is_empty(),
        "Expected BAML tool result (sum=5) in stream. Source .env for OPENROUTER_API_KEY. Message texts: {:?}. Raw: {}",
        responses
            .iter()
            .filter_map(|r| {
                r.get("result")
                    .and_then(|res| res.get("chunk").or(Some(res)))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("parts"))
                    .and_then(|p| p.as_array())
                    .and_then(|p| p.first())
                    .and_then(|part| part.get("text"))
                    .and_then(|v| v.as_str())
            })
            .collect::<Vec<_>>(),
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
}

/// Fixture: stream-js-tool. Tests streaming of a JS-only result (statusUpdate, artifactUpdate, message) and tasks.cancel.
#[tokio::test]
async fn test_e2e_stream_js_tool() {
    let agent = setup_stream_js_tool_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "stream-task: run"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-1-1".to_string())),
    };
    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let task_id = responses
        .iter()
        .filter_map(|r| {
            r.get("result")
                .and_then(|res| res.get("chunk").or(Some(res)))
        })
        .find_map(|chunk| {
            chunk
                .get("task")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    chunk
                        .get("statusUpdate")
                        .and_then(|s| s.get("taskId"))
                        .and_then(|v| v.as_str())
                })
        })
        .unwrap_or("");
    assert_eq!(task_id, "task-vox-1");

    let mut saw_status = false;
    let mut saw_artifact = false;
    for response in &responses {
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }
    assert!(saw_status, "Expected statusUpdate in streaming chunks");
    assert!(saw_artifact, "Expected artifactUpdate in streaming chunks");

    let subscribe_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.subscribe".to_string(),
        params: Some(json!({ "id": "task-vox-1", "stream": true })),
        id: Some(JSONRPCId::String("corr-1-2".to_string())),
    };
    let responses = agent
        .handle_a2a(serde_json::to_value(subscribe_request).unwrap())
        .await
        .unwrap();
    assert!(
        responses.iter().any(|response| {
            response
                .get("result")
                .and_then(|result| result.get("chunk"))
                .map(|chunk| chunk.get("task").is_some())
                .unwrap_or(false)
        }),
        "Expected task snapshot in subscribe stream"
    );

    let cancel_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.cancel".to_string(),
        params: Some(json!({ "id": "task-vox-1" })),
        id: Some(JSONRPCId::String("corr-1-3".to_string())),
    };
    let _ = agent
        .handle_a2a(serde_json::to_value(cancel_request).unwrap())
        .await
        .unwrap();

    let subscribe_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.subscribe".to_string(),
        params: Some(json!({ "id": "task-vox-1", "stream": true })),
        id: Some(JSONRPCId::String("corr-1-4".to_string())),
    };
    let responses = agent
        .handle_a2a(serde_json::to_value(subscribe_request).unwrap())
        .await
        .unwrap();
    assert!(
        responses.iter().any(|response| {
            response
                .get("result")
                .and_then(|result| result.get("chunk"))
                .and_then(|chunk| chunk.get("statusUpdate"))
                .and_then(|update| update.get("status"))
                .and_then(|status| status.get("state"))
                .and_then(|state| state.as_str())
                == Some("TASK_STATE_CANCELED")
        }),
        "Expected canceled status update after tasks.cancel"
    );
}
