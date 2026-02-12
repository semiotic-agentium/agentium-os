//! Common test utilities and shared modules.

pub use crate::support::tools::*;
#[cfg(feature = "falkordb-tests")]
mod falkordb;
#[cfg(feature = "falkordb-tests")]
pub use falkordb::{start_falkordb, wait_for_falkordb};
mod a2a_test_helpers;
pub use a2a_test_helpers::{
    chunk_content, first_message_text_from_stream, first_task_id_from_stream, send_stream_request,
    user_message,
};
mod test_tools;
pub use test_tools::{
    AddNumbersInput, AddNumbersOutput, AddNumbersTool, DelayedResponseTool, UppercaseTool,
    WeatherTool,
};

// Fixture helpers
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;
use tokio::sync::Mutex;

use baml_rt::A2aAgent;
use baml_rt::QuickJSConfig;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::quickjs_bridge::QuickJSBridge;

pub fn fixture_path(relative_path: &str) -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join(relative_path)
}

pub fn agent_fixture(name: &str) -> PathBuf {
    fixture_path(&format!("agents/{}", name))
}

/// Ensure fixture TypeScript runtime declarations are up to date.
/// Runs the builder's regen_fixtures binary once per test process.
pub fn ensure_fixture_runtime_types() {
    static REGEN_FIXTURES: Once = Once::new();
    REGEN_FIXTURES.call_once(|| {
        let output = crate::support::cli::CliHarness::new()
            .regen_fixtures_command()
            .output()
            .expect("run regen_fixtures");
        if !output.status.success() {
            panic!(
                "regen_fixtures failed: stdout={}, stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    });
}

pub fn setup_baml_runtime(schema_path: &str) -> Arc<Mutex<BamlRuntimeManager>> {
    let mut manager = BamlRuntimeManager::new().expect("Should create manager");
    manager
        .load_schema(schema_path)
        .expect("Should load schema");
    Arc::new(Mutex::new(manager))
}

pub fn setup_baml_runtime_manager(schema_path: &str) -> BamlRuntimeManager {
    let mut manager = BamlRuntimeManager::new().expect("Should create manager");
    manager
        .load_schema(schema_path)
        .expect("Should load schema");
    manager
}

pub fn setup_baml_runtime_manager_default() -> BamlRuntimeManager {
    setup_baml_runtime_manager(
        workspace_root()
            .join("baml_src")
            .to_str()
            .expect("Workspace baml_src path should be valid"),
    )
}

pub fn setup_baml_runtime_default() -> Arc<Mutex<BamlRuntimeManager>> {
    setup_baml_runtime(
        workspace_root()
            .join("baml_src")
            .to_str()
            .expect("Workspace baml_src path should be valid"),
    )
}

pub fn setup_baml_runtime_from_fixture(fixture_name: &str) -> Arc<Mutex<BamlRuntimeManager>> {
    let agent_dir = agent_fixture(fixture_name);
    assert!(
        agent_dir.join("baml_src").exists(),
        "{} fixture must have baml_src directory",
        fixture_name
    );
    setup_baml_runtime(agent_dir.to_str().expect("Fixture path should be valid"))
}

/// QuickJS config for tests: short max_attempts so effect-gated poll doesn't hang (LLM fixtures).
fn quickjs_config_for_tests() -> QuickJSConfig {
    QuickJSConfig::new().with_max_attempts_ms(Some(15_000)) // 15s instead of 30 min
}

pub async fn setup_bridge(baml_manager: Arc<Mutex<BamlRuntimeManager>>) -> QuickJSBridge {
    use baml_rt_core::ids::AgentId;
    use uuid::Uuid;
    // Generate a temporary agent_id for test context
    let temp_agent_id = AgentId::from_uuid(baml_rt_core::ids::UuidId::new(Uuid::new_v4()));
    let config = quickjs_config_for_tests();
    let mut bridge = QuickJSBridge::new_with_config(baml_manager, temp_agent_id, config)
        .await
        .expect("Create QuickJS bridge");
    bridge
        .register_baml_functions()
        .await
        .expect("Register BAML functions");
    bridge
}

pub fn require_api_key() -> String {
    let _ = dotenvy::dotenv();
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .expect("OPENROUTER_API_KEY environment variable must be set");
    assert!(!api_key.is_empty(), "OPENROUTER_API_KEY must not be empty");
    api_key
}

pub fn ensure_baml_src_exists() -> bool {
    let baml_src = workspace_root().join("baml_src");
    if !baml_src.exists() {
        println!("Skipping test: baml_src directory not found");
        return false;
    }
    true
}

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("test-support crate should be under crates/")
        .to_path_buf()
}

/// Asserts that a tool is visible in QuickJS (either as a JS tool in `__js_tools` or as a Rust tool via `openToolSession`).
/// When checking Rust tools, pass `scope` so `openToolSession` has a valid invocation token.
pub async fn assert_tool_registered_in_js(
    bridge: &mut QuickJSBridge,
    tool_name: &str,
    scope: Option<&baml_rt_core::context::InvocationScope>,
) {
    let js_code = format!(
        r#"
        (async () => {{
            const jsTools = globalThis.__js_tools || {{}};
            if (typeof jsTools["{}"] === 'function') {{
                return JSON.stringify({{
                    toolExists: true,
                    source: "js"
                }});
            }}
            try {{
                const session = await openToolSession("{}", __baml_invocation_token);
                return JSON.stringify({{
                    toolExists: true,
                    source: "rust",
                    sessionId: session.sessionId
                }});
            }} catch (error) {{
                return JSON.stringify({{
                    toolExists: false,
                    error: error.toString()
                }});
            }}
        }})()
        "#,
        tool_name, tool_name
    );
    let result = bridge.evaluate(scope, &js_code).await.unwrap_or_else(|e| {
        panic!(
            "Tool '{}' registration check failed: evaluate returned error (includes raw response if parse failed): {}",
            tool_name, e
        );
    });
    let obj = result.as_object().expect("Expected object");
    let tool_exists = obj
        .get("toolExists")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let error_detail = obj
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("(no error detail)");
    assert!(
        tool_exists,
        "Tool '{}' should be registered in QuickJS. Error: {}. Full result: {:?}",
        tool_name, error_detail, obj
    );
}

/// Builds an A2aAgent for contract tests: stream-baml-tool fixture, CalculatorTool,
/// test QuickJS config. Call `ensure_fixture_runtime_types()` before this if not already done.
/// Returns the agent; tests create scope via `InvocationScope::synthetic_message(agent.agent_id().clone())`.
pub async fn setup_stream_baml_tool_agent_for_contract(init_js: Option<&str>) -> A2aAgent {
    let mut baml_manager = BamlRuntimeManager::new().expect("create manager");
    let agent_dir = agent_fixture("stream-baml-tool");
    baml_manager
        .load_schema(agent_dir.to_str().expect("fixture path valid"))
        .expect("load schema");
    baml_manager
        .register_tool(CalculatorTool)
        .await
        .expect("register CalculatorTool");
    let config = QuickJSConfig::new().with_max_attempts_ms(Some(15_000));
    let mut builder = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(config);
    if let Some(js) = init_js {
        builder = builder.with_init_js(js);
    }
    builder.build().await.expect("build A2aAgent")
}

/// Asserts the invocation result contract: value is an object, has no "success" wrapper,
/// and has "steps" (plan) or "result"/"formatted" (tool output). Panics with CONTRACT VIOLATION message on failure.
pub fn assert_result_contract_actual_result(val: &serde_json::Value) {
    assert!(
        val.is_object(),
        "CONTRACT VIOLATION: Expected actual result (object), got: {:?}",
        val
    );
    let obj = val.as_object().unwrap();
    assert!(
        !obj.contains_key("success"),
        "CONTRACT VIOLATION: Result must not be wrapped in success object, got: {:?}",
        val
    );
    let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
    let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
    assert!(
        has_steps || has_tool_output,
        "CONTRACT VIOLATION: Expected object with 'steps' or 'result'/'formatted', got: {:?}",
        val
    );
}
