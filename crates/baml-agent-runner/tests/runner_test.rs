//! Tests for agent runner binary

use async_trait::async_trait;
use baml_rt::A2aRequestHandler;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::tools::BamlTool;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::effects::EffectBus;
use baml_rt_core::ids::{AgentId, UuidId};
#[cfg(feature = "falkordb-tests")]
use baml_rt_provenance::{
    AgentType, FalkorDbProvenanceConfig, FalkorDbProvenanceWriter, ProvEvent,
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter,
};
use baml_rt_tools::bundles::BundleType;
use flate2::Compression;
use flate2::write::GzEncoder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tar::Builder;
#[cfg(feature = "falkordb-tests")]
use testcontainers::GenericImage;
#[cfg(feature = "falkordb-tests")]
use testcontainers::core::ContainerPort;
#[cfg(feature = "falkordb-tests")]
use testcontainers::runners::AsyncRunner;
#[cfg(feature = "falkordb-tests")]
use text_to_cypher::core::execute_cypher_query;
use tokio::sync::Semaphore;
#[cfg(any(feature = "falkordb-tests", feature = "llm-tests"))]
use tokio::time::{Duration, sleep, timeout};
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

use test_support::common::{
    CalculatorTool, agent_fixture, ensure_baml_src_exists, ensure_fixture_runtime_types,
    workspace_root,
};
use test_support::support::cli::CliHarness;

fn e2e_serial_gate() -> &'static Semaphore {
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    GATE.get_or_init(|| Semaphore::new(1))
}

#[cfg(feature = "falkordb-tests")]
#[derive(Clone)]
struct StrictProvenanceWriter {
    inner: Arc<FalkorDbProvenanceWriter>,
}

#[cfg(feature = "falkordb-tests")]
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

#[cfg(feature = "falkordb-tests")]
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

#[cfg(feature = "falkordb-tests")]
async fn start_falkordb() -> Option<(testcontainers::ContainerAsync<GenericImage>, String)> {
    let image = GenericImage::new("falkordb/falkordb", "latest")
        .with_exposed_port(ContainerPort::Tcp(6379));
    let container = match image.start().await {
        Ok(container) => container,
        Err(err) => {
            panic!("start falkordb container: {err}");
        }
    };
    let mut attempts = 0;
    let host_port = loop {
        match container.get_host_port_ipv4(6379).await {
            Ok(port) => break port,
            Err(err) => {
                attempts += 1;
                if attempts > 25 {
                    panic!("get falkordb port: {err}");
                }
                sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let connection = format!("falkor://127.0.0.1:{host_port}");
    Some((container, connection))
}

#[cfg(feature = "falkordb-tests")]
async fn wait_for_falkordb(connection: &str, graph: &str) {
    sleep(Duration::from_secs(1)).await;
    let mut attempts = 0;
    loop {
        match execute_cypher_query("RETURN 1", graph, connection, false).await {
            Ok(_) => return,
            Err(err) => {
                let error_message = err.to_string();
                attempts += 1;
                if attempts > 120 {
                    panic!("falkordb did not become ready; last error: {error_message}");
                }
            }
        }
        sleep(Duration::from_secs(1)).await;
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

    // Copy baml_src from fixture (runtime loads directly from baml_src)
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
        return Err("Fixture agent dist not found".into());
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

fn extract_chunks(responses: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    responses
        .iter()
        .filter_map(|r| {
            r.get("result")
                .and_then(|res| res.get("chunk").or(Some(res)))
        })
        .collect()
}

#[cfg(feature = "llm-tests")]
fn extract_message_texts<'a>(chunks: &'a [&serde_json::Value]) -> Vec<&'a str> {
    chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .get("message")
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|p| p.first())
                .and_then(|part| part.get("text"))
                .and_then(|v| v.as_str())
        })
        .collect()
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
        .with_effect_emitter(Arc::new(EffectBus::new()))
        .build()
        .await
        .unwrap()
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
        .with_effect_emitter(Arc::new(EffectBus::new()))
        .build()
        .await
        .unwrap()
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
        .with_effect_emitter(Arc::new(EffectBus::new()))
        .build()
        .await
        .expect("build packaged A2A agent");

    (agent, extract_dir)
}

#[cfg(feature = "falkordb-tests")]
async fn setup_conversational_context_auto_agent(
    connection: String,
    graph: String,
) -> (baml_rt::A2aAgent, Arc<FalkorDbProvenanceWriter>) {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("conversational-context-auto").await;
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    let provenance = Arc::new(FalkorDbProvenanceWriter::new(
        FalkorDbProvenanceConfig::new(connection, graph),
    ));
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
        .with_effect_emitter(Arc::new(EffectBus::new()))
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
            message: user_message("e2e-invoke-1", "compute 2+3"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-1",
    );
    let outcome = agent
        .handle_a2a(serde_json::to_value(request).expect("request json"))
        .await;

    match outcome {
        Ok(responses) => {
            let chunks = extract_chunks(&responses);
            let texts = extract_message_texts(&chunks);
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
            } else {
                assert!(
                    has_sum,
                    "Expected packaged A2A invocation to produce computed result. Texts: {:?}. Raw: {}",
                    texts, pretty
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
        message: user_message("vox-1", "compute 2+3"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = send_message_request(params, "corr-1-1");
    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let chunks = extract_chunks(&responses);
    let texts = extract_message_texts(&chunks);
    let text = texts
        .iter()
        .find(|t| t.contains("sum=5"))
        .copied()
        .unwrap_or("");
    assert!(
        !text.is_empty(),
        "Expected BAML tool result (sum=5) in stream. Source .env for OPENROUTER_API_KEY. Message texts: {:?}. Raw: {}",
        texts,
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
}

/// Fixture: stream-js-tool. Tests streaming of a JS-only result (statusUpdate, artifactUpdate, message).
#[tokio::test]
async fn test_e2e_stream_js_tool() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let agent = setup_stream_js_tool_agent().await;

    let params = SendMessageRequest {
        message: user_message("vox-1", "stream-task: run"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    let request = send_message_request(params, "corr-1-1");
    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let chunks = extract_chunks(&responses);
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

    let _ = task_id;
}

#[cfg(feature = "falkordb-tests")]
#[tokio::test]
async fn test_e2e_conversational_context_auto_via_provenance() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    let (_container, connection) = start_falkordb().await.expect("start falkordb");
    let graph = format!(
        "runner_conv_ctx_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis()
    );
    wait_for_falkordb(&connection, &graph).await;
    eprintln!("conversational-context-auto: setup start");
    let (agent, provenance_reader) = timeout(
        Duration::from_secs(90),
        setup_conversational_context_auto_agent(connection, graph),
    )
    .await
    .expect("agent setup timed out");
    eprintln!("conversational-context-auto: setup complete");
    let per_turn_timeout = Duration::from_secs(45);

    let first_turn = SendMessageRequest {
        message: user_message("vox-auto-1", "Remember the codeword ORBIT and compute 2+3"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    eprintln!("conversational-context-auto: first turn start");
    let first_response = timeout(
        per_turn_timeout,
        agent.handle_a2a(
            serde_json::to_value(send_message_request(first_turn, "corr-201-1")).unwrap(),
        ),
    )
    .await
    .expect("first turn timed out")
    .unwrap();
    eprintln!("conversational-context-auto: first turn complete");
    let first_chunks = extract_chunks(&first_response);
    let first_texts = extract_message_texts(&first_chunks);
    assert!(
        first_texts
            .iter()
            .any(|text| text.contains("Computed result is 5")),
        "Expected first turn to run tool and return computed result. Texts: {:?}",
        first_texts
    );

    // FalkorDB writes may lag slightly behind turn completion; wait until turn-1
    // conversation context is queryable before issuing turn-2 memory read.
    let context_id = ContextId::new(1, 1);
    let mut history_ready = false;
    for _ in 0..50 {
        let messages = provenance_reader
            .context_messages(&context_id, Some(10))
            .await
            .unwrap_or_default();
        if messages.len() >= 2 {
            history_ready = true;
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }
    assert!(
        history_ready,
        "Expected FalkorDB-backed conversation history to contain turn-1 messages before turn-2"
    );

    let second_turn = SendMessageRequest {
        message: user_message(
            "vox-auto-2",
            "What codeword did I ask you to remember? Reply with just the codeword.",
        ),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: std::collections::HashMap::new(),
    };
    eprintln!("conversational-context-auto: second turn start");
    let second_response = timeout(
        per_turn_timeout,
        agent.handle_a2a(
            serde_json::to_value(send_message_request(second_turn, "corr-201-2")).unwrap(),
        ),
    )
    .await
    .expect("second turn timed out")
    .unwrap();
    eprintln!("conversational-context-auto: second turn complete");
    let second_chunks = extract_chunks(&second_response);
    let second_texts = extract_message_texts(&second_chunks);
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
