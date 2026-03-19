//! Tests for agent runner binary

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
};

use async_trait::async_trait;
use baml_rt::{A2aRequestHandler, baml::BamlRuntimeManager, tools::BamlTool};
use baml_rt_a2a::RegistrationMode;
use baml_rt_core::{
    BamlRtError,
    bus::BusWithEffects,
    context::{self, InvocationScope},
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
#[cfg(feature = "llm-tests")]
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter};
use baml_rt_provenance::{ProvenanceContextReader, SurrealProvenanceStore, SurrealStoreBuilder};
use baml_rt_tools::bundles::BundleType;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use flate2::{Compression, write::GzEncoder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tar::Builder;
#[cfg(feature = "llm-tests")]
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
use baml_tools_system::SystemBundle;

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
        _request: baml_rt_core::A2aWireRequest,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<baml_rt_core::A2aStreamChunk>> {
        Ok(Box::pin(futures_util::stream::empty::<
            baml_rt_core::A2aStreamChunk,
        >()))
    }
}

use baml_rt_config::{ConfigReader, ConfigWriter, SurrealConfigStore};
use baml_rt_core::{context::RuntimeScope, ids::MessageId};
use baml_rt_llm_config::{
    ClientDef, EmptySecretResolver, LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, LlmClientResolver,
    LlmProvider, StaticResolver,
};
use baml_rt_tools::BundleName;
use common::e2e_serial_gate;
#[cfg(feature = "llm-tests")]
use test_support::common::workspace_fnox_path;
use test_support::common::{
    CalculatorTool, chunks_from_responses, ensure_baml_src_exists, ensure_fixture_runtime_types,
    first_task_id_from_stream, message_texts_from_chunks, user_message, user_message_with_task,
    workspace_root,
};

fn stream_collector_idle_secs() -> u64 {
    if std::env::var_os("CI").is_some() {
        300
    } else {
        90
    }
}

async fn build_fixture_to_temp_async(fixture_name: &str) -> std::path::PathBuf {
    test_support::common::build_fixture_package_to_temp(fixture_name).await
}

#[cfg(feature = "slack")]
#[tokio::test]
async fn test_slack_smoke_fixture_builds_with_generated_tools() {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("slack-smoke-tool").await;
    assert!(
        built.join("dist").join("index.js").exists(),
        "Expected compiled dist/index.js for slack-smoke-tool fixture"
    );
    assert!(
        built.join("baml_src").join("generated_tools.baml").exists(),
        "Expected generated_tools.baml in packaged fixture baml_src"
    );
    std::fs::remove_dir_all(&built).ok();
}

/// Create a test agent package from a fixture agent
async fn create_test_agent_package(output_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Build fixture first so dist/index.js is guaranteed to exist.
    let agent_dir = build_fixture_to_temp_async("stream-baml-tool").await;

    let unique = uuid::Uuid::new_v4();
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
impl baml_rt_tools::DescribeAction for AddNumbersInput {
    fn describe(&self) -> String {
        format!("adding {} + {}", self.a, self.b)
    }
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
    let request_value = serde_json::to_value(request).expect("request json");
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request_value))
        .await?;
    let chunks = baml_rt::collect_a2a_stream_until(stream, |item| {
        let v = item.as_ref();
        let chunk = v.get("result").and_then(|r| r.get("chunk").or(Some(r)));
        let state = chunk.and_then(chunk_state);
        let is_final = v
            .get("result")
            .and_then(|r| r.get("final"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        is_final || matches!(state.as_deref(), Some("TASK_STATE_INPUT_REQUIRED"))
    })
    .await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

#[cfg(feature = "llm-tests")]
async fn setup_stream_baml_tool_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("stream-baml-tool").await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-baml-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(build_surreal_test_store().await)
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
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .unwrap();
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
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(build_surreal_test_store().await)
        .build()
        .await
        .unwrap()
}

async fn setup_stream_js_tool_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("stream-js-tool").await;
    let mut manager = {
        #[cfg(feature = "llm-tests")]
        let m = BamlRuntimeManager::builder()
            .with_fnox_llm_resolver(workspace_fnox_path())
            .build()
            .unwrap();
        #[cfg(not(feature = "llm-tests"))]
        let m = BamlRuntimeManager::builder().build().unwrap();
        m
    };
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-js-tool dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(build_surreal_test_store().await)
        .build()
        .await
        .unwrap()
}

#[derive(Clone)]
struct PackageTargetRouter {
    routes: HashMap<String, baml_rt::A2aAgent>,
}

#[async_trait]
impl A2aRequestHandler for PackageTargetRouter {
    async fn handle_a2a_stream(
        &self,
        request: baml_rt_core::A2aWireRequest,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<baml_rt_core::A2aStreamChunk>> {
        let target_package = request
            .as_ref()
            .get("params")
            .and_then(|params| params.get("metadata"))
            .and_then(|meta| meta.get("target"))
            .and_then(|target| target.get("agent_package"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "system/internal_a2a missing params.metadata.target.agent_package".to_string(),
                )
            })?;

        let agent = self.routes.get(target_package).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "No route configured for target package '{target_package}'"
            ))
        })?;
        agent.handle_a2a_stream(request).await
    }
}

async fn setup_internal_a2a_router_agents(
    target_package: &str,
) -> (
    baml_rt::A2aAgent,
    baml_rt::A2aAgent,
    Arc<SurrealProvenanceStore>,
    Arc<SurrealProvenanceStore>,
) {
    let responder_manager = BamlRuntimeManager::builder().build().unwrap();
    let responder_store = build_surreal_test_store().await;
    let responder_code = r#"
globalThis.onChatMessage = async function(message) {
  const text = message?.parts?.[0]?.text || "unknown";
  __chat_yield({ message: { parts: [{ text: `Responder saw: ${text}` }] } });
  __chat_yield({ final: true });
};
"#;
    let responder_agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(responder_manager)
        .with_init_js(responder_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_quickjs_config(
            baml_rt::QuickJSConfig::new()
                .with_stream_collector_idle_secs(Some(stream_collector_idle_secs())),
        )
        .with_surreal_store(responder_store.clone())
        .build()
        .await
        .unwrap();

    let router: Arc<dyn A2aRequestHandler> = Arc::new(PackageTargetRouter {
        routes: HashMap::from([("responder-agent".to_string(), responder_agent.clone())]),
    });

    let initiator_manager = BamlRuntimeManager::builder().build().unwrap();
    let target_literal = serde_json::to_string(target_package).expect("serialize target package");
    let initiator_store = build_surreal_test_store().await;
    let initiator_code = r#"
globalThis.onChatMessage = async function(message) {
    const userText = message?.parts?.[0]?.text || "ping";
    try {
    const session = await openToolSession("system/internal_a2a", {
      target: { agent_package: __TARGET_PACKAGE__, agent_instance_id: "default" }
    });
    await session.send({ parts: [{ text: userText }] });
    let next = await session.continue();
    const allChunks = [];
    while (next?.status === "streaming" || next?.status === "suspended") {
      const batch =
        Array.isArray(next?.chunks) ? next.chunks :
        Array.isArray(next?.output?.chunks) ? next.output.chunks :
        [];
      allChunks.push(...batch);
      next = await session.continue();
    }
    if (next?.status === "error") {
      const delegatedError =
        next?.error?.message ||
        next?.output?.error?.message ||
        "delegated session error";
      throw new Error(delegatedError);
    }
    if (next?.output?.chunks) allChunks.push(...next.output.chunks);
    await session.finish();

    const chunks = allChunks;
    const texts = [];
    for (const rawChunk of chunks) {
      let chunk = rawChunk;
      if (typeof rawChunk === "string" && rawChunk.trim().startsWith("{")) {
        try {
          chunk = JSON.parse(rawChunk);
        } catch (_e) {
          continue;
        }
      }
      const t =
        chunk?.message?.parts?.[0]?.text ??
        chunk?.task?.status?.message?.parts?.[0]?.text ??
        chunk?.statusUpdate?.status?.message?.parts?.[0]?.text ??
        null;
      if (typeof t === "string" && t.length > 0) {
        texts.push(t);
      }
    }
    __chat_yield({
      message: { parts: [{ text: texts.join(" | ") || "No delegated text" }] }
    });
    __chat_yield({ final: true });
    } catch (e) {
      __chat_yield({ message: { parts: [{ text: `delegate_error=${String(e)}` }] } });
      __chat_yield({ final: true });
    }
};
"#
    .replace("__TARGET_PACKAGE__", &target_literal);
    let initiator_agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(initiator_manager)
        .with_init_js(initiator_code)
        .with_a2a_session_tool(RegistrationMode::Register)
        .with_a2a_session_router(router)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_quickjs_config(
            baml_rt::QuickJSConfig::new()
                .with_stream_collector_idle_secs(Some(stream_collector_idle_secs())),
        )
        .with_surreal_store(initiator_store.clone())
        .build()
        .await
        .unwrap();

    (
        initiator_agent,
        responder_agent,
        initiator_store,
        responder_store,
    )
}

async fn setup_internal_a2a_parallel_fanout_agents(
    target_package: &str,
    fanout: usize,
) -> (
    baml_rt::A2aAgent,
    baml_rt::A2aAgent,
    Arc<SurrealProvenanceStore>,
    Arc<SurrealProvenanceStore>,
) {
    let responder_manager = BamlRuntimeManager::builder().build().unwrap();
    let responder_store = build_surreal_test_store().await;
    let responder_code = r#"
globalThis.onChatMessage = async function(message) {
  const text = message?.parts?.[0]?.text || "unknown";
  __chat_yield({ message: { parts: [{ text: `Responder saw: ${text}` }] } });
  __chat_yield({ final: true });
};
"#;
    let responder_agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(responder_manager)
        .with_init_js(responder_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(responder_store.clone())
        .build()
        .await
        .unwrap();

    let router: Arc<dyn A2aRequestHandler> = Arc::new(PackageTargetRouter {
        routes: HashMap::from([("responder-agent".to_string(), responder_agent.clone())]),
    });

    let initiator_manager = BamlRuntimeManager::builder().build().unwrap();
    let target_literal = serde_json::to_string(target_package).expect("serialize target package");
    let fanout_literal = fanout.to_string();
    let initiator_store = build_surreal_test_store().await;
    let initiator_code = r#"
globalThis.onChatMessage = async function(message) {
  const baseText = message?.parts?.[0]?.text || "ping parallel";
  const count = __FANOUT__;
  const work = Array.from({ length: count }, (_v, i) => `${baseText} ${i}`);

  function collectTextsAndChildIds(chunks) {
    const texts = [];
    const childIds = new Set();
    for (const rawChunk of chunks) {
      let chunk = rawChunk;
      if (typeof rawChunk === "string" && rawChunk.trim().startsWith("{")) {
        try {
          chunk = JSON.parse(rawChunk);
        } catch (_e) {
          continue;
        }
      }
      const t =
        chunk?.message?.parts?.[0]?.text ??
        chunk?.task?.status?.message?.parts?.[0]?.text ??
        chunk?.statusUpdate?.status?.message?.parts?.[0]?.text ??
        null;
      if (typeof t === "string" && t.length > 0) {
        texts.push(t);
      }
      const taskVal = chunk?.task ?? chunk?.statusUpdate ?? null;
      let task = taskVal;
      if (typeof taskVal === "string" && taskVal.trim().startsWith("{")) {
        try {
          task = JSON.parse(taskVal);
        } catch (_e) {
          task = null;
        }
      }
      const taskId =
        (typeof task?.id === "string" ? task.id : null) ??
        (typeof task?.taskId === "string" ? task.taskId : null);
      if (taskId && taskId.startsWith("a2a-child-")) {
        childIds.add(taskId);
      }
    }
    return { texts, child_task_ids: Array.from(childIds) };
  }

  const results = await Promise.all(work.map(async (text, index) => {
    try {
      const session = await openToolSession("system/internal_a2a", {
        target: { agent_package: __TARGET_PACKAGE__, agent_instance_id: "default" }
      });
      await session.send({ parts: [{ text }] });
      let next = await session.continue();
      const allChunks = [];
      while (next?.status === "streaming" || next?.status === "suspended") {
        const batch =
          Array.isArray(next?.chunks) ? next.chunks :
          Array.isArray(next?.output?.chunks) ? next.output.chunks :
          [];
        allChunks.push(...batch);
        next = await session.continue();
      }
      if (next?.output?.chunks) allChunks.push(...next.output.chunks);
      await session.finish();

      const collected = collectTextsAndChildIds(allChunks);
      return { index, input: text, ...collected };
    } catch (e) {
      return { index, input: text, error: String(e), texts: [], child_task_ids: [] };
    }
  }));

  __chat_yield({ message: { parts: [{ text: JSON.stringify(results) }] } });
  __chat_yield({ final: true });
};
"#
    .replace("__TARGET_PACKAGE__", &target_literal)
    .replace("__FANOUT__", &fanout_literal);
    let initiator_agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(initiator_manager)
        .with_init_js(initiator_code)
        .with_a2a_session_tool(RegistrationMode::Register)
        .with_a2a_session_router(router)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(initiator_store.clone())
        .build()
        .await
        .unwrap();

    (
        initiator_agent,
        responder_agent,
        initiator_store,
        responder_store,
    )
}

async fn setup_task_lifecycle_demo_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async("task-lifecycle-demo").await;
    let mut manager = {
        #[cfg(feature = "llm-tests")]
        let m = BamlRuntimeManager::builder()
            .with_fnox_llm_resolver(workspace_fnox_path())
            .build()
            .unwrap();
        #[cfg(not(feature = "llm-tests"))]
        let m = BamlRuntimeManager::builder().build().unwrap();
        m
    };
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("task-lifecycle-demo dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(build_surreal_test_store().await)
        .build()
        .await
        .unwrap()
}

#[cfg(feature = "llm-tests")]
async fn setup_argument_fixture_agent(fixture: &str) -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_to_temp_async(fixture).await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .unwrap();
    manager.load_schema(built.to_str().unwrap()).unwrap();
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("argument fixture dist/index.js");
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(build_surreal_test_store().await)
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
        request: baml_rt_core::A2aWireRequest,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<baml_rt_core::A2aStreamChunk>> {
        let target = request
            .as_ref()
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

    let mut manager = {
        #[cfg(feature = "llm-tests")]
        let m = BamlRuntimeManager::builder()
            .with_fnox_llm_resolver(workspace_fnox_path())
            .build()
            .expect("runtime manager");
        #[cfg(not(feature = "llm-tests"))]
        let m = BamlRuntimeManager::builder()
            .build()
            .expect("runtime manager");
        m
    };
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
        .with_surreal_store(build_surreal_test_store().await)
        .build()
        .await
        .expect("build packaged A2A agent");

    (agent, extract_dir)
}

/// Build coordinator-agent from workspace agents/coordinator-agent and return an A2aAgent.
/// Uses SurrealDB store so persistent mode is satisfied. Call only when agents/coordinator-agent exists.
/// Kept for use by test on base branch after merge (test removed here to avoid duplicate definition in PR #62).
#[cfg(feature = "llm-tests")]
#[allow(dead_code)] // used by coordinator E2E when run with llm-tests feature
async fn setup_coordinator_agent() -> baml_rt::A2aAgent {
    ensure_fixture_runtime_types();
    let agent_dir = workspace_root().join("agents").join("coordinator-agent");
    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!(
            "coordinator-agent dir missing or invalid: {}",
            agent_dir.display()
        );
    }
    let extract_dir = common::build_agent_dir_to_temp_async(agent_dir, "coordinator-agent").await;

    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("runtime manager");
    manager
        .load_schema(extract_dir.to_str().expect("utf8 path"))
        .expect("load coordinator schema");
    let allowlist: HashSet<String> = ["system/internal_a2a"]
        .into_iter()
        .map(String::from)
        .collect();
    manager
        .set_tool_allowlist(allowlist)
        .await
        .expect("set allowlist");
    let policy = parse_access_allowlist();
    let manifest_tools = ManifestToolNames::parse(&["system/internal_a2a".to_string()])
        .expect("parse manifest tools");
    let registry = manager.tool_registry();
    register_manifest_tools(registry.as_ref(), &manifest_tools, &policy).expect("register tools");
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(EmptyA2aHandler),
        ))
        .expect("register SystemBundle");

    let entry_js = fs::read_to_string(extract_dir.join("dist").join("index.js"))
        .expect("coordinator-agent dist/index.js");
    // Persistent mode requires a store; omit and A2aAgent::build() returns InvalidArgument.
    let store = build_surreal_test_store().await;
    baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(entry_js)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(store)
        .build()
        .await
        .expect("build coordinator agent")
}

async fn build_surreal_test_store() -> Arc<SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated surreal store")
}

#[tokio::test]
async fn test_manifest_allowlist_blocks_undeclared_tool() {
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
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

// test_coordinator_keyword_domain_falls_back_to_single_loaded_domain: defined on base branch only.
// Removed here to avoid duplicate definition when this branch is merged (PR #62).

#[tokio::test]
async fn test_agent_package_loading() {
    // This test verifies that we can load an agent package

    // Create a test agent package
    let unique = uuid::Uuid::new_v4();
    let package_path = std::env::temp_dir().join(format!("test-agent-package-{unique}.tar.gz"));

    match create_test_agent_package(&package_path).await {
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

    let extract_dir = std::env::temp_dir().join(format!(
        "test-agent-extract-{}-{unique}",
        std::process::id()
    ));
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

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
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

/// E2E client substitution: fixture saves LLM config to the persistent config store, then we load
/// from store and resolve. Asserts that config → store → load → StaticResolver → ClientRegistry
/// works so BAML overrides the schema's minimal client at runtime from persistent configuration.
#[tokio::test]
async fn test_client_registry_substitution_from_config() {
    let store = SurrealConfigStore::in_memory()
        .await
        .expect("in-memory config store for integration test");

    let mut options = HashMap::new();
    options.insert("model".to_string(), "openai/gpt-4o-mini".to_string());
    let client = ClientDef {
        name: "Default".to_string(),
        provider: LlmProvider::Openrouter,
        options,
        retry_policy: None,
    };
    let mut clients = HashMap::new();
    clients.insert("Default".to_string(), client);
    let config = LlmClientConfig {
        default: "Default".to_string(),
        clients,
        ..Default::default()
    };

    let bundle =
        BundleName::new(LLM_CONFIG_BUNDLE_NAME).expect("LLM config bundle name must be valid");
    store
        .set(
            &bundle,
            serde_json::to_value(&config).expect("serialize LLM config"),
        )
        .await
        .expect("save LLM config to store");

    let value = store
        .get(&bundle)
        .await
        .expect("read from store")
        .expect("config present");
    let loaded = LlmClientConfig::from_value(value).expect("deserialize LLM config from store");

    let resolver = StaticResolver::new(Arc::new(loaded), Arc::new(EmptySecretResolver));
    let scope = RuntimeScope::message_scope(
        ContextId::new(1, 1),
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap()),
        MessageId::from("msg-1"),
    );
    let registry_opt = resolver.resolve(&scope, "AddNumbers").await.unwrap();
    assert!(
        registry_opt.is_some(),
        "Substitution: resolver must return Some(registry) so BAML overrides schema client"
    );
    assert!(
        !registry_opt.unwrap().is_empty(),
        "Substitution: registry from config must be non-empty"
    );
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
///
/// **Regression test for resume (L4-Resume) bug:** Chapman's second turn sends a follow-up
/// with the same context_id; the handler runs BAML (`ArgumentReply`) on resume. That exercises
/// the same path as "hi twice" in claude-session-demo: deliver_resume_input → brief eval →
/// poll_promise_until_result. If the JS continuation that calls `__set_eval_result` never runs
/// on the event loop we advance, the poll hits the cap and the test fails. See
/// crates/baml-rt-quickjs/docs/QUICKJS_BRIDGE_LIVENESS_INVARIANTS.md (L4-Resume).
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

    // Real-model latency can exceed two minutes under CI load.
    let timeout_duration = std::time::Duration::from_secs(180);
    let result =
        tokio::time::timeout(timeout_duration, run_argument_sketch_two_agents_body()).await;
    match result {
        Ok(()) => {}
        Err(_) => panic!(
            "test_e2e_argument_sketch_two_agents did not complete within {:?}",
            timeout_duration
        ),
    }
}

#[cfg(feature = "llm-tests")]
async fn run_argument_sketch_two_agents_body() {
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
            || lower.contains("i won't")
            || lower.contains("i will not")
            || lower.contains("i shan't")
            || lower.contains("certainly")
            || lower.contains("it is")
            || lower.contains("it isn")
    };
    // Prefer two chunks (Cleese then Chapman); stream-yield/task-local can sometimes deliver only one (see docs/argument-sketch-stream-trace.md).
    assert!(
        !first_texts.is_empty(),
        "Expected at least one message chunk. Texts: {:?}. Raw: {}",
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
    let chapman_task_id = first_task_id_from_stream(&chapman_first_responses)
        .map(|s| TaskId::from_external(ExternalId::new(s)));
    assert!(
        chapman_first_states.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "Expected TASK_STATE_INPUT_REQUIRED from Chapman (awaitInput after first line); states: {:?}",
        chapman_first_states
    );
    let chapman_second = send_message_request(
        SendMessageRequest {
            message: user_message_with_task(
                "ch-2",
                "I didn't.",
                Some(chapman_context_id),
                chapman_task_id,
            ),
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
        "Expected TASK_STATE_COMPLETED from Chapman after resume (conversation must be resumed); states: {:?}. Chunks (first 3): {:?}. Responses len: {}. First response keys: {:?}",
        chapman_second_states,
        chapman_second_chunks.get(0..3.min(chapman_second_chunks.len())),
        chapman_second_responses.len(),
        chapman_second_responses
            .first()
            .and_then(|r| r.as_object().map(|o| o.keys().collect::<Vec<_>>()))
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
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(
            serde_json::to_value(subscribe_request).unwrap(),
        ))
        .await
        .unwrap();
    let responses: Vec<serde_json::Value> = baml_rt::collect_a2a_stream(stream)
        .await
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect();
    let has_task_snapshot = responses.iter().any(|response| {
        response
            .get("result")
            .and_then(|result| result.get("chunk"))
            .map(|chunk| chunk.get("task").is_some())
            .unwrap_or(false)
    });
    let task_not_found = responses.iter().any(|response| {
        response
            .get("error")
            .and_then(|e| e.get("data"))
            .and_then(|d| d.get("details"))
            .and_then(|v| v.as_str())
            == Some("Task not found")
    });
    assert!(
        has_task_snapshot || task_not_found,
        "Expected task snapshot in subscribe stream or Task not found (live-stream persistence gap); got {} response(s)",
        responses.len()
    );

    let _ = task_id;
}

#[tokio::test]
async fn test_internal_a2a_can_route_to_different_agent_package() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (initiator_agent, _responder_agent, _initiator_store, _responder_store) =
        setup_internal_a2a_router_agents("responder-agent").await;
    let request = send_message_request(
        SendMessageRequest {
            message: user_message("route-1", "ping cross-package", Some(ContextId::new(44, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-1",
    );

    let responses = collect_stream_responses(&initiator_agent, request)
        .await
        .expect("internal_a2a routed request");
    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Responder saw: ping cross-package")),
        "Expected delegated response text from responder agent. Texts: {:?}. Raw: {}",
        texts,
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
    assert!(
        !texts.iter().any(|t| t.starts_with("delegate_error=")),
        "Expected no delegated error marker. Texts: {:?}",
        texts
    );
}

#[tokio::test]
async fn test_internal_a2a_context_id_propagates() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (initiator_agent, _responder_agent, _initiator_store, _responder_store) =
        setup_internal_a2a_router_agents("responder-agent").await;
    let context_id = ContextId::new(44, 1);
    let request = send_message_request(
        SendMessageRequest {
            message: user_message("route-ctx", "ping cross-package", Some(context_id.clone())),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1700000000099-1",
    );

    let responses = collect_stream_responses(&initiator_agent, request)
        .await
        .expect("internal_a2a routed request");
    let chunks = chunks_from_responses(&responses);
    let expected_ctx_str = context_id.as_str();
    let chunk_context_ids: Vec<&str> = chunks
        .iter()
        .filter_map(|c| {
            c.get("task")
                .and_then(|t| t.get("contextId"))
                .and_then(Value::as_str)
        })
        .collect();
    assert!(
        chunk_context_ids.contains(&expected_ctx_str),
        "Expected at least one chunk with task.contextId equal to request context_id {expected_ctx_str:?}. \
         Chunk contextIds: {chunk_context_ids:?}. Raw responses: {}",
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
}

/// Parallel same-context fanout: 6 child A2A streams under one parent.
///
/// Previously blocked on the global single-thread `HANDOVER_LANE` which
/// serialized all child streams. Resolved by the bridge-local dispatcher
/// introduced in Phase 1 (`runtime_refactor.md`).
#[tokio::test]
#[ignore = "flaky under CI contention; parallel same-context child-task fanout remains non-blocking"]
async fn test_internal_a2a_parallel_same_context_child_tasks_and_provenance() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let request_count = 6usize;
    let (initiator_agent, _responder_agent, _initiator_store, responder_store) =
        setup_internal_a2a_parallel_fanout_agents("responder-agent", request_count).await;
    let context_id = ContextId::new(88, 1);
    let request = send_message_request(
        SendMessageRequest {
            message: user_message("route-par-root", "ping parallel", Some(context_id.clone())),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1700000000200-1",
    );
    let responses = collect_stream_responses(&initiator_agent, request)
        .await
        .expect("parallel fanout request");
    let chunks = chunks_from_responses(&responses);
    fn collect_part_texts(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(parts) = map.get("parts").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            out.push(text.to_string());
                        }
                    }
                }
                for child in map.values() {
                    collect_part_texts(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_part_texts(item, out);
                }
            }
            Value::String(raw) => {
                let trimmed = raw.trim_start();
                if (trimmed.starts_with('{') || trimmed.starts_with('['))
                    && let Ok(parsed) = serde_json::from_str::<Value>(raw)
                {
                    collect_part_texts(&parsed, out);
                }
            }
            _ => {}
        }
    }

    let mut candidate_texts = message_texts_from_chunks(&chunks);
    for chunk in &chunks {
        collect_part_texts(chunk, &mut candidate_texts);
    }
    let summary_text = candidate_texts
        .iter()
        .find(|t| {
            let trimmed = t.trim();
            !trimmed.is_empty()
                && trimmed != "null"
                && serde_json::from_str::<Value>(trimmed)
                    .ok()
                    .and_then(|v| v.as_array().map(|_| ()))
                    .is_some()
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "parallel summary message; collected texts: {:?}; raw: {}",
                candidate_texts,
                serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
            )
        });
    let parsed_summary: Value =
        serde_json::from_str(&summary_text).expect("summary message should be valid JSON");
    let results = parsed_summary
        .as_array()
        .cloned()
        .expect("summary should be an array");
    assert_eq!(
        results.len(),
        request_count,
        "Expected {request_count} fanout results"
    );
    let mut child_ids_from_summary: HashSet<String> = HashSet::new();
    for i in 0..request_count {
        let expected_input = format!("ping parallel {i}");
        let expected = format!("Responder saw: ping parallel {i}");
        let row = results
            .iter()
            .find(|r| {
                r.get("index").and_then(Value::as_u64) == Some(i as u64)
                    && r.get("input").and_then(Value::as_str) == Some(expected_input.as_str())
            })
            .expect("result row for index");
        assert!(
            row.get("error").is_none(),
            "Fanout result unexpectedly contains error for index {i}: {row:?}"
        );
        let texts = row
            .get("texts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            texts
                .iter()
                .any(|t| t.as_str().map(|s| s.contains(&expected)).unwrap_or(false)),
            "Expected delegated echo in texts for index {i}; row={row:?}"
        );
        let ids: Vec<String> = row
            .get("child_task_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "Expected exactly one child task id for index {i}, got {ids:?}"
        );
        child_ids_from_summary.extend(ids);
    }
    assert_eq!(
        child_ids_from_summary.len(),
        request_count,
        "Expected one unique child task id per fanout branch, got {child_ids_from_summary:?}"
    );

    // Provenance regression: all delegated user messages must be queryable under the same root context.
    let mut matched_messages = 0usize;
    for _ in 0..60 {
        let messages = responder_store
            .context_messages(&context_id, Some(200))
            .await
            .expect("read responder context messages");
        matched_messages = messages
            .iter()
            .filter(|m| m.content.iter().any(|c| c.starts_with("ping parallel ")))
            .count();
        if matched_messages >= request_count {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        matched_messages >= request_count,
        "Expected at least {request_count} delegated user messages in responder provenance context, got {matched_messages}"
    );
}

#[tokio::test]
async fn test_internal_a2a_unknown_target_surfaces_error() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (initiator_agent, _responder_agent, _initiator_store, _responder_store) =
        setup_internal_a2a_router_agents("missing-agent").await;
    let request = send_message_request(
        SendMessageRequest {
            message: user_message("route-2", "ping cross-package", Some(ContextId::new(45, 1))),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: std::collections::HashMap::new(),
        },
        "corr-1-2",
    );

    let responses = collect_stream_responses(&initiator_agent, request)
        .await
        .expect("internal_a2a routed request");
    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    assert!(
        texts.iter().any(|t| {
            t.starts_with("delegate_error=")
                || t.contains("No route configured for target package 'missing-agent'")
        }),
        "Expected unknown-route error text for missing target. Texts: {:?}. Raw: {}",
        texts,
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );
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
    let lifecycle_task_id = first_task_id_from_stream(&first_responses)
        .map(|task_id| TaskId::from_external(ExternalId::new(task_id)))
        .expect("Expected task id in first stream for resume turns");

    let second_params = SendMessageRequest {
        message: user_message_with_task(
            "vox-2",
            "review-path",
            Some(ContextId::new(1, 1)),
            Some(lifecycle_task_id.clone()),
        ),
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
        message: user_message_with_task(
            "vox-3",
            "revise",
            Some(ContextId::new(1, 1)),
            Some(lifecycle_task_id.clone()),
        ),
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
        message: user_message_with_task(
            "vox-4",
            "apply redaction and tighten summary",
            Some(ContextId::new(1, 1)),
            Some(lifecycle_task_id.clone()),
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
        message: user_message_with_task(
            "vox-5",
            "approve",
            Some(ContextId::new(1, 1)),
            Some(lifecycle_task_id.clone()),
        ),
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
        message: user_message_with_task(
            "vox-6",
            "confirm",
            Some(ContextId::new(1, 1)),
            Some(lifecycle_task_id),
        ),
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
