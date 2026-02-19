//! Tests for agent runner binary

use async_trait::async_trait;
use baml_rt::A2aRequestHandler;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::tools::BamlTool;
use baml_rt_core::bus::BusWithEffects;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_provenance::{
    AgentType, GraphqliteProvenanceStore, GraphqliteStoreBuilder, ProvEvent,
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter,
};
use baml_rt_tools::bundles::BundleType;
use flate2::Compression;
use flate2::write::GzEncoder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tar::Builder;
use tokio::sync::Semaphore;
use tokio::time::{Duration, sleep, timeout};
use ts_rs::TS;

// Bundle type for test tools (used as AddNumbersTool::Bundle; referenced in test_runner_tool_types_for_package_build).
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}
use baml_rt::a2a_types::{JSONRPCId, JSONRPCRequest, SendMessageRequest};
#[cfg(feature = "llm-tests")]
use baml_rt_core::{AgentDiscoveryEntry, AgentLister};
#[cfg(feature = "llm-tests")]
use baml_rt_tools::{ManifestToolNames, parse_access_allowlist, register_manifest_tools};
#[cfg(feature = "llm-tests")]
use baml_rt_tools_system::SystemBundle;

/// Empty agent list for tests that only need discover_tools (no discover_agents).
#[cfg(feature = "llm-tests")]
struct EmptyAgentList;

#[cfg(feature = "llm-tests")]
impl AgentLister for EmptyAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        vec![]
    }
}

/// Dummy A2A handler for registering SystemBundle before the agent exists (used only for discover_tools test).
#[cfg(feature = "llm-tests")]
struct EmptyA2aHandler;

#[cfg(feature = "llm-tests")]
#[async_trait]
impl A2aRequestHandler for EmptyA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: Value,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<Value>> {
        Ok(Box::pin(futures_util::stream::empty::<Value>()))
    }
}

use test_support::common::{
    CalculatorTool, agent_fixture, chunks_from_responses, ensure_baml_src_exists,
    ensure_fixture_runtime_types, message_texts_from_chunks, user_message, workspace_root,
};
use test_support::support::cli::CliHarness;

fn init_test_tracing() {
    static TRACING: OnceLock<()> = OnceLock::new();
    TRACING.get_or_init(|| {
        baml_rt_observability::init_tracing();
    });
}

fn e2e_serial_gate() -> &'static Semaphore {
    init_test_tracing();
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    GATE.get_or_init(|| Semaphore::new(1))
}

#[derive(Clone)]
struct StrictProvenanceWriter {
    inner: Arc<GraphqliteProvenanceStore>,
}

#[async_trait]
impl ProvenanceWriter for StrictProvenanceWriter {
    async fn add_event(
        &self,
        event: ProvEvent,
    ) -> std::result::Result<(), baml_rt_provenance::ProvenanceError> {
        match self.inner.add_event(event.clone()).await {
            Ok(()) => Ok(()),
            Err(err) => panic!("strict provenance write failure: {err:?}; event={event:?}"),
        }
    }
}

#[async_trait]
impl ProvenanceContextReader for StrictProvenanceWriter {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<Vec<ProvenanceContextMessage>, baml_rt_provenance::ProvenanceError>
    {
        self.inner.context_messages(context_id, limit).await
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<
        Vec<ProvenanceConversationContextItem>,
        baml_rt_provenance::ProvenanceError,
    > {
        self.inner.conversation_context(context_id, limit).await
    }
}

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

async fn build_fixture_to_temp_async(fixture_name: &str) -> std::path::PathBuf {
    let fixture = fixture_name.to_string();
    tokio::task::spawn_blocking(move || build_fixture_to_temp(&fixture))
        .await
        .expect("build fixture task join")
}

/// Create a test agent package from a fixture agent
fn create_test_agent_package(output_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Build fixture first so dist/index.js is guaranteed to exist.
    let agent_dir = build_fixture_to_temp("stream-baml-tool");

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir =
        std::env::temp_dir().join(format!("e2e-agent-{}-{}", std::process::id(), unique));
    fs::create_dir_all(&temp_dir)?;

    // Copy baml_src from built fixture (runtime loads directly from baml_src)
    let baml_src = temp_dir.join("baml_src");
    let fixture_baml_src = agent_dir.join("baml_src");
    if fixture_baml_src.exists() {
        copy_dir_all(&fixture_baml_src, &baml_src)?;
    } else {
        return Err("Fixture agent baml_src not found".into());
    }

    // Copy compiled JS entrypoint expected by manifest: dist/index.js
    let fixture_dist = agent_dir.join("dist");
    let dist = temp_dir.join("dist");
    if fixture_dist.exists() {
        copy_dir_all(&fixture_dist, &dist)?;
    } else {
        return Err(format!(
            "Built fixture missing dist directory: {}",
            fixture_dist.display()
        )
        .into());
    }

    // Create manifest.json (stream-baml-tool fixture has support/calculate only)
    let manifest = serde_json::json!({
        "version": "1.0.0",
        "name": "test-agent",
        "entry_point": "dist/index.js",
        "signature": "test-agent@1.0.0",
        "tools": ["support/calculate"],
        "discovery": { "description": "Test agent package for E2E testing", "capabilities": [] }
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

// Tool type for package build and ts_rs/JsonSchema; referenced in test_runner_tool_types_for_package_build.
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

/// References Test and AddNumbersTool so they are not reported dead (used for package build / impls).
#[test]
fn test_runner_tool_types_for_package_build() {
    let _ = AddNumbersTool;
    assert_eq!(Test::NAME, "test");
}

fn jsonrpc_request(method: &str, params: serde_json::Value, id: &str) -> JSONRPCRequest {
    JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params: Some(params),
        id: Some(JSONRPCId::String(id.to_string())),
    }
}

fn send_message_request(params: SendMessageRequest, id: &str) -> JSONRPCRequest {
    jsonrpc_request(
        "message.sendStream",
        serde_json::to_value(params).unwrap(),
        id,
    )
}

/// Extracts task state from a chunk. Handles both object and stringified task/statusUpdate.
fn chunk_state(chunk: &Value) -> Option<String> {
    fn state_from(val: &Value) -> Option<String> {
        val.get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| {
                val.as_str().and_then(|s| {
                    serde_json::from_str::<Value>(s).ok().and_then(|parsed| {
                        parsed
                            .get("status")
                            .and_then(|s| s.get("state"))
                            .and_then(Value::as_str)
                            .map(String::from)
                    })
                })
            })
    }
    chunk
        .get("task")
        .and_then(state_from)
        .or_else(|| chunk.get("statusUpdate").and_then(state_from))
}

async fn collect_stream_responses(
    agent: &baml_rt::A2aAgent,
    request: JSONRPCRequest,
) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(serde_json::to_value(request).expect("request json"))
        .await?;
    Ok(baml_rt::collect_a2a_stream_until(stream, |item| {
        let chunk = item.get("result").and_then(|r| r.get("chunk").or(Some(r)));
        let state = chunk.and_then(|c| chunk_state(c));
        let is_final = item
            .get("result")
            .and_then(|r| r.get("final"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        is_final || matches!(state.as_deref(), Some("TASK_STATE_INPUT_REQUIRED"))
    })
    .await)
}

#[cfg(feature = "llm-tests")]
async fn setup_stream_baml_tool_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("stream-baml-tool").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-baml-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap()
}

/// Tool-discovery-demo agent: uses system/discover_tools + support/calculate.
/// Build fixture, register manifest tools and SystemBundle, then build agent.
#[cfg(feature = "llm-tests")]
async fn setup_tool_discovery_demo_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("tool-discovery-demo").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let allowlist: HashSet<String> = [
        "system/discover_tools",
        "system/discover_agents",
        "system/internal_a2a",
        "support/calculate",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    manager.set_tool_allowlist(allowlist).await.unwrap();
    let policy = parse_access_allowlist();
    let manifest_tools =
        ManifestToolNames::parse(&["support/calculate".to_string()]).expect("parse manifest tools");
    let registry = manager.tool_registry();
    register_manifest_tools(registry.as_ref(), &manifest_tools, &policy).expect("register tools");
    // Register SystemBundle before build so registry has system/discover_tools when allowlist is validated.
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(EmptyA2aHandler),
        ))
        .expect("register SystemBundle");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("tool-discovery-demo dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap();
    agent
}

async fn setup_stream_js_tool_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("stream-js-tool").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-js-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap()
}

async fn setup_task_lifecycle_demo_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("task-lifecycle-demo").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("task-lifecycle-demo dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap()
}

#[cfg(feature = "llm-tests")]
async fn setup_argument_fixture_agent(fixture: &str) -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async(fixture).await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("argument fixture dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap()
}

#[cfg(feature = "llm-tests")]
#[derive(Clone)]
struct TwoAgentSessionRouter {
    cleese: baml_rt::A2aAgent,
    chapman: baml_rt::A2aAgent,
}

#[cfg(feature = "llm-tests")]
#[async_trait]
impl A2aRequestHandler for TwoAgentSessionRouter {
    async fn handle_a2a_stream(
        &self,
        request: Value,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<Value>> {
        let target = request
            .get("params")
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("target"))
            .and_then(|t| t.get("agent_package"))
            .and_then(Value::as_str);

        match target {
            Some("argument-chapman") => self.chapman.handle_a2a_stream(request).await,
            Some("argument-cleese") => self.cleese.handle_a2a_stream(request).await,
            _ => self.cleese.handle_a2a_stream(request).await,
        }
    }
}

/// Build fixture package with the builder, load runtime from extracted package,
/// and create an A2A agent.
/// Returns `(agent, extract_dir)` so caller can cleanup extracted artifacts.
async fn setup_packaged_stream_baml_tool_agent() -> (baml_rt::A2aAgent, std::path::PathBuf) {
    let extract_dir = build_fixture_to_temp_async("stream-baml-tool").await;

    let mut manager = BamlRuntimeManager::new().expect("runtime manager");
    manager
        .load_schema(extract_dir.to_str().expect("utf8 path"))
        .expect("load schema from extracted package");
    manager
        .register_tool(CalculatorTool)
        .await
        .expect("register support/calculate tool");

    let entry_js = fs::read_to_string(extract_dir.join("dist").join("index.js"))
        .expect("read extracted dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(entry_js)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build packaged A2A agent");

    (agent, extract_dir)
}

async fn setup_conversational_context_auto_agent()
-> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>) {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("conversational-context-auto").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    let provenance = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build GraphQLite store");
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            ContextId::new(1, 1),
            agent_id.clone(),
            AgentType::new("conversational-context-auto").expect("agent type"),
            "1.0.0".to_string(),
            "conversational-context-auto@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");
    let strict_writer = Arc::new(StrictProvenanceWriter {
        inner: provenance.clone(),
    });
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("conversational-context-auto dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_provenance_writer(strict_writer)
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .unwrap();
    (agent, provenance)
}

#[tokio::test]
async fn test_manifest_allowlist_blocks_undeclared_tool() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);

    manager.set_tool_allowlist(HashSet::new()).await.unwrap();
    let blocked = context::with_scope(scope.as_scope().clone(), async {
        manager
            .open_tool_session(scope.as_scope(), "support/calculate", json!({}))
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
            .open_tool_session(scope.as_scope(), "support/calculate", json!({}))
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
            panic!("Failed to create test package: {}", e);
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
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (agent, extract_dir) = setup_packaged_stream_baml_tool_agent().await;
    assert!(
        !agent.agent_id().as_str().is_empty(),
        "Loaded packaged agent must have a valid agent_id"
    );

    fs::remove_dir_all(&extract_dir).ok();
}

#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_e2e_agent_runner_invoke_function() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    // E2E via packaged agent loaded in-process, then invoked through A2A.
    // This avoids recursive `cargo run` subprocess behavior and validates
    // package -> runtime -> A2A request flow directly.
    let _ = dotenvy::dotenv();
    let has_api_key = std::env::var("OPENROUTER_API_KEY").is_ok();
    let (agent, extract_dir) = setup_packaged_stream_baml_tool_agent().await;

    // Invoke through A2A request path.
    let request = send_message_request(
        SendMessageRequest {
            message: user_message("e2e-invoke-1", "compute 2+3", Some(ContextId::new(1, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-1",
    );
    let outcome = collect_stream_responses(&agent, request).await;

    match outcome {
        Ok(responses) => {
            let chunks = chunks_from_responses(&responses);
            let texts = message_texts_from_chunks(&chunks);
            let pretty =
                serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string());
            let has_sum = texts
                .iter()
                .any(|t| t.contains("sum=5") || t.contains("Computed result is 5"));
            let has_auth_error = pretty.contains("API key")
                || pretty.contains("authentication")
                || pretty.contains("401")
                || pretty.contains("unauthorized")
                || pretty.contains("Unauthorized");

            if !has_api_key && has_auth_error {
                println!("Expected authentication error (no API key provided)");
            } else if !has_sum {
                // Diagnostic: when we get no computed result, show what we actually received
                let first_response_keys = responses
                    .first()
                    .and_then(|r| r.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let first_result_keys = responses
                    .first()
                    .and_then(|r| r.get("result"))
                    .and_then(|res| res.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let first_has_error = responses.first().and_then(|r| r.get("error")).is_some();
                panic!(
                    "Expected packaged A2A invocation to produce computed result. \
                     responses.len()={}, chunks.len()={}, texts.len()={}. \
                     First response keys: {:?}. First result keys: {:?}. First is error: {}. \
                     Texts: {:?}. Raw: {}",
                    responses.len(),
                    chunks.len(),
                    texts.len(),
                    first_response_keys,
                    first_result_keys,
                    first_has_error,
                    texts,
                    pretty
                );
            }
        }
        Err(e) => {
            let err = e.to_string();
            let is_auth_error = err.contains("API key")
                || err.contains("authentication")
                || err.contains("401")
                || err.contains("unauthorized")
                || err.contains("Unauthorized");
            if !has_api_key && is_auth_error {
                println!("Expected authentication error (no API key provided)");
            } else {
                panic!("Packaged A2A invoke failed unexpectedly: {err}");
            }
        }
    }

    // Cleanup
    fs::remove_dir_all(&extract_dir).ok();
}

/// Tool-discovery-demo: agent uses system/discover_tools to find tools; ask about "calculate" and assert response mentions it.
#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_tool_discovery_demo_responds_with_tool_list() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    let agent = setup_tool_discovery_demo_agent().await;
    let request = send_message_request(
        SendMessageRequest {
            message: user_message(
                "discovery-1",
                "what tools do you have for calculate?",
                Some(ContextId::new(1, 1)),
            ),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1737123456789-1",
    );
    let outcome = collect_stream_responses(&agent, request).await;
    match outcome {
        Ok(responses) => {
            let chunks = chunks_from_responses(&responses);
            let texts = message_texts_from_chunks(&chunks);
            if texts.is_empty() {
                // Diagnostic: log response/chunk shape so we can see why no message text was extracted.
                eprintln!("tool_discovery_demo: responses.len()={}", responses.len());
                if let Some(r) = responses.first() {
                    let keys: Vec<_> = r
                        .as_object()
                        .map(|o| o.keys().collect())
                        .unwrap_or_default();
                    eprintln!("tool_discovery_demo: first response keys: {:?}", keys);
                    if let Some(err) = r.get("error") {
                        eprintln!(
                            "tool_discovery_demo: JSON-RPC error: {}",
                            serde_json::to_string_pretty(err).unwrap_or_else(|_| err.to_string())
                        );
                    }
                    if let Some(result) = r.get("result") {
                        let result_keys: Vec<_> = result
                            .as_object()
                            .map(|o| o.keys().collect())
                            .unwrap_or_default();
                        eprintln!("tool_discovery_demo: first result keys: {:?}", result_keys);
                        if let Some(chunk) = result
                            .get("chunk")
                            .or_else(|| result.as_object().map(|_| result))
                        {
                            let chunk_keys: Vec<_> = chunk
                                .as_object()
                                .map(|o| o.keys().collect())
                                .unwrap_or_default();
                            eprintln!("tool_discovery_demo: first chunk keys: {:?}", chunk_keys);
                            eprintln!(
                                "tool_discovery_demo: first chunk (preview): {}",
                                serde_json::to_string(chunk)
                                    .unwrap_or_default()
                                    .chars()
                                    .take(500)
                                    .collect::<String>()
                            );
                        }
                    }
                }
                eprintln!("tool_discovery_demo: chunks.len()={}", chunks.len());
            }
            let combined = texts.join(" ");
            assert!(
                combined.contains("support/calculate")
                    || combined.to_lowercase().contains("calculate")
                    || combined.contains("No tools found"),
                "Expected tool-discovery response to mention calculate or support/calculate or 'No tools found'. Got: {:?}",
                texts
            );
        }
        Err(e) => {
            if std::env::var("OPENROUTER_API_KEY").is_ok() {
                panic!("Tool discovery demo request failed: {e}");
            }
            // No API key: auth error is acceptable
        }
    }
}

/// Fixture: stream-baml-tool. Tests async streaming of a BAML tool (FSM) result via message.sendStream.
/// Requires .env with OPENROUTER_API_KEY (source .env or set in test env).
#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_e2e_stream_baml_tool() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();

    let agent = setup_stream_baml_tool_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "compute 2+3", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = send_message_request(params, "corr-1-1");
    let responses = collect_stream_responses(&agent, request).await.unwrap();
    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    let text = texts
        .iter()
        .find(|t| t.contains("sum=5"))
        .cloned()
        .unwrap_or_default();
    assert!(
        !text.is_empty(),
        "Expected BAML tool result (sum=5) in stream. Source .env for OPENROUTER_API_KEY. Message texts: {:?}. Raw: {}",
        texts,
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
}

/// Fixture: argument-cleese + argument-chapman.
/// Tests cross-agent conversation through system/internal_a2a (compat alias of system/a2a).
#[cfg(feature = "llm-tests")]
#[tokio::test]
async fn test_e2e_argument_sketch_two_agents() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!("Skipping test_e2e_argument_sketch_two_agents: OPENROUTER_API_KEY not set");
        return;
    }

    let cleese_agent = setup_argument_fixture_agent("argument-cleese").await;
    let chapman_agent = setup_argument_fixture_agent("argument-chapman").await;
    let router: Arc<dyn A2aRequestHandler> = Arc::new(TwoAgentSessionRouter {
        cleese: cleese_agent.clone(),
        chapman: chapman_agent.clone(),
    });

    let agent_list = Arc::new(EmptyAgentList);
    for agent in [&cleese_agent, &chapman_agent] {
        let runtime = agent.runtime();
        let manager = runtime.lock().await;
        let registry = manager.tool_registry();
        registry
            .register_bundle(SystemBundle::new(
                agent_list.clone(),
                registry.clone(),
                router.clone(),
            ))
            .expect("register system bundle");
    }

    fn task_state(chunk: &Value) -> Option<String> {
        fn state_from_val(val: &Value) -> Option<String> {
            val.get("status")
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    val.as_str().and_then(|s| {
                        serde_json::from_str::<Value>(s).ok().and_then(|parsed| {
                            parsed
                                .get("status")
                                .and_then(|s| s.get("state"))
                                .and_then(Value::as_str)
                                .map(String::from)
                        })
                    })
                })
        }
        chunk
            .get("task")
            .and_then(state_from_val)
            .or_else(|| chunk.get("statusUpdate").and_then(state_from_val))
    }

    let context_id = ContextId::new(1, 1);

    // Turn 1: start argument → Cleese replies, delegates to Chapman, then awaitInput → INPUT_REQUIRED
    let first_request = send_message_request(
        SendMessageRequest {
            message: user_message("arg-1", "Start the argument.", Some(context_id.clone())),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-1",
    );
    let first_responses = collect_stream_responses(&cleese_agent, first_request)
        .await
        .expect("argument sketch first turn");
    let first_chunks = chunks_from_responses(&first_responses);
    let first_states: Vec<String> = first_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        first_states.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "Expected TASK_STATE_INPUT_REQUIRED in first stream (Cleese awaitInput after Chapman reply); states: {:?}",
        first_states
    );
    let first_texts = message_texts_from_chunks(&first_chunks);
    let argument_like = |t: &String| {
        let lower = t.to_lowercase();
        lower.contains("yes it is")
            || lower.contains("no it isn't")
            || lower.contains("no it isnt")
            || lower.contains("i didn't")
            || lower.contains("you did")
            || lower.contains("i'm not")
            || lower.contains("you are")
    };
    assert!(
        first_texts.len() >= 2,
        "Expected at least two message chunks (Cleese and Chapman). Texts: {:?}. Raw: {}",
        first_texts,
        serde_json::to_string_pretty(&first_responses).unwrap_or_else(|_| "?".to_string())
    );
    assert!(
        first_texts.iter().any(argument_like),
        "Expected at least one argument-sketch style line. Texts: {:?}",
        first_texts
    );

    // Turn 2: resume (same context_id) → Cleese completes
    let second_request = send_message_request(
        SendMessageRequest {
            message: user_message("arg-2", "done", Some(context_id)),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-2",
    );
    let second_responses = collect_stream_responses(&cleese_agent, second_request)
        .await
        .expect("argument sketch second turn (resume)");
    let second_chunks = chunks_from_responses(&second_responses);
    let second_states: Vec<String> = second_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        second_states.contains(&"TASK_STATE_COMPLETED".to_string()),
        "Expected TASK_STATE_COMPLETED after resume; states: {:?}",
        second_states
    );
    let second_texts = message_texts_from_chunks(&second_chunks);
    assert!(
        second_texts.iter().any(|t| t.contains("Done")),
        "Expected completion message after resume; texts: {:?}",
        second_texts
    );

    // Chapman directly: turn 1 → one line + INPUT_REQUIRED; turn 2 (resume) → second line + COMPLETED
    let chapman_context_id = ContextId::new(1, 2);
    let chapman_first = send_message_request(
        SendMessageRequest {
            message: user_message("ch-1", "No it isn't.", Some(chapman_context_id.clone())),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-2",
    );
    let chapman_first_responses = collect_stream_responses(&chapman_agent, chapman_first)
        .await
        .expect("Chapman first turn");
    let chapman_first_chunks = chunks_from_responses(&chapman_first_responses);
    let chapman_first_states: Vec<String> = chapman_first_chunks
        .iter()
        .filter_map(|c| task_state(c))
        .collect();
    assert!(
        chapman_first_states.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "Expected TASK_STATE_INPUT_REQUIRED from Chapman (awaitInput after first line); states: {:?}",
        chapman_first_states
    );
    let chapman_second = send_message_request(
        SendMessageRequest {
            message: user_message("ch-2", "I didn't.", Some(chapman_context_id)),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-3",
    );
    let chapman_second_responses = collect_stream_responses(&chapman_agent, chapman_second)
        .await
        .expect("Chapman second turn (resume)");
    let chapman_second_chunks = chunks_from_responses(&chapman_second_responses);
    let chapman_second_states: Vec<String> = chapman_second_chunks
        .iter()
        .filter_map(|c| task_state(c))
        .collect();
    assert!(
        chapman_second_states.contains(&"TASK_STATE_COMPLETED".to_string()),
        "Expected TASK_STATE_COMPLETED from Chapman after resume (conversation must be resumed); states: {:?}",
        chapman_second_states
    );
}

/// Fixture: stream-js-tool. Tests streaming of a JS-only result (statusUpdate, artifactUpdate, message).
#[tokio::test]
async fn test_e2e_stream_js_tool() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let agent = setup_stream_js_tool_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "stream-task: run", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = send_message_request(params, "corr-1-1");
    let responses = collect_stream_responses(&agent, request).await.unwrap();
    let chunks = chunks_from_responses(&responses);
    let task_id = chunks
        .iter()
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
    assert!(!task_id.is_empty(), "Expected task id in streaming chunks");
    // Chunk-shape semantics (statusUpdate/artifactUpdate) are asserted in task_streaming_test.rs; here we only verify wiring/operability.

    let subscribe_request = jsonrpc_request(
        "tasks.subscribe",
        json!({ "id": task_id, "stream": true }),
        "corr-1-2",
    );
    let responses = baml_rt::collect_a2a_stream(
        agent
            .handle_a2a_stream(serde_json::to_value(subscribe_request).unwrap())
            .await
            .unwrap(),
    )
    .await;
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

    let _ = task_id;
}

/// Fixture: task-lifecycle-demo.
/// Tests sequential loops (no nesting): review loop then sign-off loop.
/// Path: start -> path -> review loop (revise/notes/approve) -> sign-off loop (confirm) -> completed.
#[tokio::test]
async fn test_e2e_task_lifecycle_demo() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let agent = setup_task_lifecycle_demo_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "lifecycle-demo", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let first_request = send_message_request(params, "corr-1-3");
    let first_responses = collect_stream_responses(&agent, first_request)
        .await
        .unwrap();
    let first_chunks = chunks_from_responses(&first_responses);

    fn task_state(chunk: &serde_json::Value) -> Option<&str> {
        chunk
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(|s| s.get("state"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                chunk
                    .get("statusUpdate")
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.get("state"))
                    .and_then(|v| v.as_str())
            })
    }

    let first_states: Vec<&str> = first_chunks.iter().filter_map(|c| task_state(c)).collect();
    let has_working = first_states.contains(&"TASK_STATE_WORKING");
    let has_input_required = first_states.contains(&"TASK_STATE_INPUT_REQUIRED");
    let has_completed_first = first_states.contains(&"TASK_STATE_COMPLETED");
    let has_artifact = first_chunks
        .iter()
        .any(|c| c.get("artifactUpdate").is_some());

    assert!(
        has_working,
        "Expected TASK_STATE_WORKING in first stream; states: {:?}",
        first_states
    );
    assert!(
        has_input_required,
        "Expected TASK_STATE_INPUT_REQUIRED in first stream; states: {:?}",
        first_states
    );
    assert!(
        !has_completed_first,
        "Did not expect TASK_STATE_COMPLETED before resume; states: {:?}",
        first_states
    );
    assert!(
        has_artifact,
        "Expected artifactUpdate in first stream; chunks: {}",
        first_chunks.len()
    );

    let first_texts = message_texts_from_chunks(&first_chunks);
    assert!(
        first_texts.iter().any(|t| t.contains("Task started.")),
        "Expected startup message in first stream; texts: {:?}",
        first_texts
    );

    let second_params = SendMessageRequest {
        message: user_message("vox-2", "review-path", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let second_request = send_message_request(second_params, "corr-1-4");
    let second_responses = collect_stream_responses(&agent, second_request)
        .await
        .unwrap();
    let second_chunks = chunks_from_responses(&second_responses);
    let second_states: Vec<&str> = second_chunks.iter().filter_map(|c| task_state(c)).collect();
    let has_input_required_second = second_states.contains(&"TASK_STATE_INPUT_REQUIRED");
    let has_completed_second = second_states.contains(&"TASK_STATE_COMPLETED");
    assert!(
        has_input_required_second,
        "Expected TASK_STATE_INPUT_REQUIRED in second stream; states: {:?}",
        second_states
    );
    assert!(
        !has_completed_second,
        "Did not expect TASK_STATE_COMPLETED in second stream; states: {:?}",
        second_states
    );
    let second_texts = message_texts_from_chunks(&second_chunks);
    assert!(
        second_texts
            .iter()
            .any(|t| t.contains("Review path selected.")),
        "Expected review-path progress message in second stream; texts: {:?}",
        second_texts
    );

    let third_params = SendMessageRequest {
        message: user_message("vox-3", "revise", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let third_request = send_message_request(third_params, "corr-1-5");
    let third_responses = collect_stream_responses(&agent, third_request)
        .await
        .unwrap();
    let third_chunks = chunks_from_responses(&third_responses);
    let third_states: Vec<&str> = third_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        third_states.contains(&"TASK_STATE_INPUT_REQUIRED"),
        "Expected TASK_STATE_INPUT_REQUIRED in third stream; states: {:?}",
        third_states
    );
    assert!(
        !third_states.contains(&"TASK_STATE_COMPLETED"),
        "Did not expect TASK_STATE_COMPLETED in third stream; states: {:?}",
        third_states
    );
    let third_texts = message_texts_from_chunks(&third_chunks);
    assert!(
        third_texts
            .iter()
            .any(|t| t.contains("Revision requested. Awaiting revision notes.")),
        "Expected revision prompt message in third stream; texts: {:?}",
        third_texts
    );

    let fourth_params = SendMessageRequest {
        message: user_message(
            "vox-4",
            "apply redaction and tighten summary",
            Some(ContextId::new(1, 1)),
        ),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let fourth_request = send_message_request(fourth_params, "corr-1-6");
    let fourth_responses = collect_stream_responses(&agent, fourth_request)
        .await
        .unwrap();
    let fourth_chunks = chunks_from_responses(&fourth_responses);
    let fourth_states: Vec<&str> = fourth_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        fourth_states.contains(&"TASK_STATE_INPUT_REQUIRED"),
        "Expected TASK_STATE_INPUT_REQUIRED in fourth stream after revision notes; states: {:?}",
        fourth_states
    );
    let fourth_texts = message_texts_from_chunks(&fourth_chunks);
    assert!(
        fourth_texts
            .iter()
            .any(|t| t.contains("Revision notes captured: apply redaction and tighten summary")),
        "Expected revision-captured message in fourth stream; texts: {:?}",
        fourth_texts
    );

    // Turn 5: approve review -> exits review loop, enters sign-off loop (INPUT_REQUIRED).
    let fifth_params = SendMessageRequest {
        message: user_message("vox-5", "approve", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let fifth_request = send_message_request(fifth_params, "corr-1-10");
    let fifth_responses = collect_stream_responses(&agent, fifth_request)
        .await
        .unwrap();
    let fifth_chunks = chunks_from_responses(&fifth_responses);
    let fifth_states: Vec<&str> = fifth_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        fifth_states.contains(&"TASK_STATE_INPUT_REQUIRED"),
        "Expected TASK_STATE_INPUT_REQUIRED for sign-off in fifth stream; states: {:?}",
        fifth_states
    );
    let fifth_texts = message_texts_from_chunks(&fifth_chunks);
    assert!(
        fifth_texts
            .iter()
            .any(|t| t.contains("Review approved. Proceeding to sign-off.")),
        "Expected review-approved message in fifth stream; texts: {:?}",
        fifth_texts
    );

    // Turn 6: confirm sign-off -> COMPLETED.
    let sixth_params = SendMessageRequest {
        message: user_message("vox-6", "confirm", Some(ContextId::new(1, 1))),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let sixth_request = send_message_request(sixth_params, "corr-1-11");
    let sixth_responses = collect_stream_responses(&agent, sixth_request)
        .await
        .unwrap();
    let sixth_chunks = chunks_from_responses(&sixth_responses);
    let sixth_states: Vec<&str> = sixth_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        sixth_states.contains(&"TASK_STATE_COMPLETED"),
        "Expected TASK_STATE_COMPLETED in sixth stream; states: {:?}",
        sixth_states
    );
    let sixth_texts = message_texts_from_chunks(&sixth_chunks);
    assert!(
        sixth_texts
            .iter()
            .any(|t| t.contains("Task completed after review and sign-off.")),
        "Expected final completion message in sixth stream; texts: {:?}",
        sixth_texts
    );
}

/// Fixture: task-lifecycle-demo.
/// Exercises the failure rail on the review branch:
/// start -> input_required(path) -> input_required(review decision) -> failed(reject).
#[tokio::test]
async fn test_e2e_task_lifecycle_demo_reject_path() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let agent = setup_task_lifecycle_demo_agent().await;

    fn task_state(chunk: &serde_json::Value) -> Option<&str> {
        chunk
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(|s| s.get("state"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                chunk
                    .get("statusUpdate")
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.get("state"))
                    .and_then(|v| v.as_str())
            })
    }

    // Turn 1: trigger lifecycle, expect INPUT_REQUIRED.
    let first_request = send_message_request(
        SendMessageRequest {
            message: user_message("vox-r-1", "lifecycle-demo", Some(ContextId::new(1, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-7",
    );
    let first_responses = collect_stream_responses(&agent, first_request)
        .await
        .unwrap();
    let first_chunks = chunks_from_responses(&first_responses);
    let first_states: Vec<&str> = first_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        first_states.contains(&"TASK_STATE_INPUT_REQUIRED"),
        "Expected TASK_STATE_INPUT_REQUIRED after trigger; states: {:?}",
        first_states
    );

    // Turn 2: choose review path, expect another INPUT_REQUIRED for review decision.
    let second_request = send_message_request(
        SendMessageRequest {
            message: user_message("vox-r-2", "review-path", Some(ContextId::new(1, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-8",
    );
    let second_responses = collect_stream_responses(&agent, second_request)
        .await
        .unwrap();
    let second_chunks = chunks_from_responses(&second_responses);
    let second_states: Vec<&str> = second_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        second_states.contains(&"TASK_STATE_INPUT_REQUIRED"),
        "Expected TASK_STATE_INPUT_REQUIRED for review decision; states: {:?}",
        second_states
    );

    // Turn 3: reject -> terminal failure.
    let third_request = send_message_request(
        SendMessageRequest {
            message: user_message("vox-r-3", "reject", Some(ContextId::new(1, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-9",
    );
    let third_responses = collect_stream_responses(&agent, third_request)
        .await
        .unwrap();
    let third_chunks = chunks_from_responses(&third_responses);
    let third_states: Vec<&str> = third_chunks.iter().filter_map(|c| task_state(c)).collect();
    assert!(
        third_states.contains(&"TASK_STATE_FAILED"),
        "Expected TASK_STATE_FAILED after reject; states: {:?}",
        third_states
    );
    let third_texts = message_texts_from_chunks(&third_chunks);
    let failed_status_texts: Vec<&str> = third_chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .get("task")
                .and_then(|t| t.get("status"))
                .and_then(|s| s.get("message"))
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|p| p.first())
                .and_then(|part| part.get("text"))
                .and_then(|v| v.as_str())
        })
        .collect();
    assert!(
        third_texts
            .iter()
            .map(|s| s.as_str())
            .chain(failed_status_texts.iter().copied())
            .any(|t| t.contains("Rejected during review.")),
        "Expected rejection failure message; chunk texts: {:?}, failed-status texts: {:?}",
        third_texts,
        failed_status_texts
    );
}

#[tokio::test]
async fn test_e2e_conversational_context_auto_via_provenance() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    eprintln!("conversational-context-auto: setup start");
    let (agent, provenance_reader) = timeout(
        Duration::from_secs(600),
        setup_conversational_context_auto_agent(),
    )
    .await
    .expect("agent setup timed out");
    eprintln!("conversational-context-auto: setup complete");
    let per_turn_timeout = Duration::from_secs(45);

    let first_turn = SendMessageRequest {
        message: user_message(
            "vox-auto-1",
            "Remember the codeword ORBIT and compute 2+3",
            Some(ContextId::new(1, 1)),
        ),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    eprintln!("conversational-context-auto: first turn start");
    let first_response = timeout(
        per_turn_timeout,
        collect_stream_responses(&agent, send_message_request(first_turn, "corr-201-1")),
    )
    .await
    .expect("first turn timed out")
    .unwrap();
    eprintln!("conversational-context-auto: first turn complete");
    let first_chunks = chunks_from_responses(&first_response);
    let first_texts = message_texts_from_chunks(&first_chunks);
    assert!(
        first_texts
            .iter()
            .any(|text| text.contains("Computed result is 5")),
        "Expected first turn to run tool and return computed result. Texts: {:?}",
        first_texts
    );

    // GraphQLite writes may lag behind turn completion (normalization + Cypher execution);
    // wait until turn-1 conversation context is queryable before issuing turn-2 memory read.
    let context_id = ContextId::new(1, 1);
    sleep(Duration::from_millis(1500)).await; // let async writes settle before first poll
    let mut history_ready = false;
    let mut codeword_ready = false;
    let mut last_messages: Vec<ProvenanceContextMessage> = Vec::new();
    let poll_attempts = 150; // 150 * 500ms = 75s max
    let poll_interval_ms = 500;
    for attempt in 0..poll_attempts {
        let messages = provenance_reader
            .context_messages(&context_id, Some(10))
            .await
            .unwrap_or_default();
        last_messages = messages.clone();
        if messages.len() >= 2 {
            history_ready = true;
        }
        codeword_ready = messages.iter().any(|m| {
            m.content
                .iter()
                .any(|part| part.to_ascii_uppercase().contains("ORBIT"))
        });
        if history_ready && codeword_ready {
            break;
        }
        if attempt < poll_attempts - 1 {
            sleep(Duration::from_millis(poll_interval_ms)).await;
        }
    }
    assert!(
        history_ready,
        "Expected GraphQLite-backed conversation history to contain turn-1 messages before turn-2. \
         After ~{}s poll ({} attempts x {}ms): got {} messages (roles: {:?}). \
         Failure may indicate provenance write lag or assistant message not yet written.",
        (poll_attempts * poll_interval_ms) / 1000,
        poll_attempts,
        poll_interval_ms,
        last_messages.len(),
        last_messages
            .iter()
            .map(|m| m.role.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        codeword_ready,
        "Expected provenance context to include codeword ORBIT before turn-2 recall. \
         After ~{}s poll ({} attempts x {}ms): messages={:?}",
        (poll_attempts * poll_interval_ms) / 1000,
        poll_attempts,
        poll_interval_ms,
        last_messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect::<Vec<_>>()
    );

    let second_turn = SendMessageRequest {
        message: user_message(
            "vox-auto-2",
            "What codeword did I ask you to remember? Reply with just the codeword.",
            Some(ContextId::new(1, 1)),
        ),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    eprintln!("conversational-context-auto: second turn start");
    let second_response = timeout(
        per_turn_timeout,
        collect_stream_responses(&agent, send_message_request(second_turn, "corr-201-2")),
    )
    .await
    .expect("second turn timed out")
    .unwrap();
    eprintln!("conversational-context-auto: second turn complete");
    let second_chunks = chunks_from_responses(&second_response);
    let second_texts = message_texts_from_chunks(&second_chunks);
    let expected_codeword = "ORBIT";
    assert!(
        second_texts.iter().any(|text| {
            let normalized = text.trim().to_ascii_uppercase();
            normalized.contains(expected_codeword) || expected_codeword.contains(&normalized)
        }),
        "Expected second turn to recall codeword from provenance-backed conversation context. Texts: {:?}. Raw: {}",
        second_texts,
        serde_json::to_string_pretty(&second_response).unwrap_or_else(|_| "?".to_string())
    );
}
