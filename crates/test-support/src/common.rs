//! Common test utilities and shared modules.

pub use crate::support::tools::*;
mod a2a_test_helpers;
pub use a2a_test_helpers::{
    chunk_content, chunks_from_responses, first_message_text_from_stream,
    first_task_id_from_stream, is_error_response, message_texts_from_chunks, send_stream_request,
    send_stream_request_with_task, user_message, user_message_with_task,
};
mod test_tools;
// Fixture helpers
use std::{fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{A2aAgent, QuickJSConfig, baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge};
use baml_rt_core::bus::{
    BusWithEffects, EffectEmitter, EffectEvent, EffectLiveness, EffectSubscriber,
};
use baml_rt_provenance::GraphqliteStoreBuilder;
pub use test_tools::{
    AddNumbersInput, AddNumbersOutput, AddNumbersTool, DelayedResponseTool, UppercaseTool,
    WeatherTool,
};
use tokio::sync::Mutex;

/// Effect subscriber that captures all emitted `EffectEvent`s into a `Vec`.
///
/// Useful in tests that need to assert planning/tool effect sequences without
/// requiring a full provenance store. Constructed via `Default::default()`.
#[derive(Default)]
pub struct CapturingEffectSubscriber {
    pub events: tokio::sync::Mutex<Vec<EffectEvent>>,
}

#[async_trait]
impl EffectSubscriber for CapturingEffectSubscriber {
    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        self.events.lock().await.push(event.clone());
        Ok(())
    }
}

/// Creates a `QuickJSBridge` wired with a `BusWithEffects` and a `CapturingEffectSubscriber`.
///
/// Returns `(bridge, capture)` — the bridge has BAML functions registered and effect
/// liveness set. The capture accumulates all emitted `EffectEvent`s for later assertion.
pub async fn make_capturing_bridge(
    agent_id: baml_rt_core::ids::AgentId,
) -> (QuickJSBridge, Arc<CapturingEffectSubscriber>) {
    let manager = Arc::new(Mutex::new(
        BamlRuntimeManager::new().expect("create BamlRuntimeManager"),
    ));
    let effect_bus = Arc::new(BusWithEffects::new());
    let capture = Arc::new(CapturingEffectSubscriber::default());
    effect_bus.subscribe_effect(capture.clone()).await;
    {
        let mut guard = manager.lock().await;
        guard.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let mut bridge = QuickJSBridge::new(manager, agent_id)
        .await
        .expect("create QuickJSBridge");
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("register BAML host helpers");
    (bridge, capture)
}

pub fn fixture_path(relative_path: &str) -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join(relative_path)
}

pub fn agent_fixture(name: &str) -> PathBuf {
    fixture_path(&format!("agents/{}", name))
}

/// Builds a fixture agent using the builder crate (no subprocess), unpacks to temp dir, returns path.
/// The extracted dir contains `dist/index.js` and `baml_src/`. Use this to load a real agent
/// (QuickJS runs the compiled TS) for A2A streaming tests. Call from tests that need full stack.
pub async fn build_fixture_package_to_temp(fixture_name: &str) -> PathBuf {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let agent_dir = agent_fixture(fixture_name);
    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!(
            "Fixture {} not found or missing baml_src at {}",
            fixture_name,
            agent_dir.display()
        );
    }
    let unique = uuid::Uuid::new_v4();
    let pid = std::process::id();
    let tar_path = std::env::temp_dir().join(format!(
        "a2a-test-{}-{}-{}.tar.gz",
        fixture_name, pid, unique
    ));
    let extract_dir = std::env::temp_dir().join(format!(
        "a2a-test-{}-extract-{}-{}",
        fixture_name, pid, unique
    ));
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).expect("create extract dir");

    baml_rt_builder::build_agent_package(&agent_dir, &tar_path)
        .await
        .unwrap_or_else(|e| panic!("build fixture {fixture_name} failed: {e}"));

    let tar_gz = fs::File::open(&tar_path).expect("open built tar");
    let tar_dec = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar_dec);
    archive.unpack(&extract_dir).expect("unpack built tar");
    let _ = fs::remove_file(&tar_path);

    let dist_index = extract_dir.join("dist").join("index.js");
    assert!(
        dist_index.exists(),
        "Built package must contain dist/index.js at {}",
        dist_index.display()
    );
    extract_dir
}

/// Builds an agent at the given path using the builder crate (no subprocess), unpacks to temp dir, returns path.
/// Use this instead of spawning `cargo run -p baml-rt-builder` to avoid Cargo lock deadlock.
pub async fn build_agent_package_to_temp(agent_dir: PathBuf, package_label: &str) -> PathBuf {
    use flate2::read::GzDecoder;
    use tar::Archive;

    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!(
            "Agent dir {} missing or invalid (no baml_src)",
            agent_dir.display()
        );
    }
    let unique = uuid::Uuid::new_v4();
    let pid = std::process::id();
    let tar_path =
        std::env::temp_dir().join(format!("runner-test-{package_label}-{pid}-{unique}.tar.gz"));
    let extract_dir = std::env::temp_dir().join(format!(
        "runner-test-{package_label}-extract-{pid}-{unique}"
    ));
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).expect("create extract dir");

    baml_rt_builder::build_agent_package(&agent_dir, &tar_path)
        .await
        .unwrap_or_else(|e| panic!("build agent {package_label} failed: {e}"));

    let tar_gz = fs::File::open(&tar_path).expect("open built tar");
    let tar_dec = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar_dec);
    archive.unpack(&extract_dir).expect("unpack built tar");
    let _ = fs::remove_file(&tar_path);

    let dist_index = extract_dir.join("dist").join("index.js");
    assert!(
        dist_index.exists(),
        "Built package must contain dist/index.js at {}",
        dist_index.display()
    );
    extract_dir
}

/// Assert that fixture TypeScript runtime declarations exist.
///
/// Scans `tests/fixtures/agents/` for directories containing `baml_src/`
/// and asserts each also has `src/baml-runtime.d.ts`. With nextest, these
/// are generated by the `regen-fixtures` setup script (see
/// `.config/nextest.toml`). For plain `cargo test`, run the binary
/// manually first: `cargo run -p baml-rt-builder --bin regen_fixtures`.
pub fn ensure_fixture_runtime_types() {
    let agents_dir = workspace_root()
        .join("tests")
        .join("fixtures")
        .join("agents");
    let mut missing = Vec::new();
    let entries = std::fs::read_dir(&agents_dir).unwrap_or_else(|e| {
        panic!(
            "Cannot read fixture agents directory {}: {e}",
            agents_dir.display()
        );
    });
    for entry in entries {
        let entry = entry.expect("read fixture entry");
        let path = entry.path();
        if path.join("baml_src").is_dir() && !path.join("src").join("baml-runtime.d.ts").exists() {
            missing.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if !missing.is_empty() {
        missing.sort();
        panic!(
            "Missing baml-runtime.d.ts for fixtures: {missing:?}\n\
             Run: cargo run -p baml-rt-builder --bin regen_fixtures"
        );
    }
}

/// Path to workspace-root fnox.toml. Use with
/// `BamlRuntimeManager::builder().with_fnox_llm_resolver(workspace_fnox_path())` so resolution
/// works regardless of test cwd (package dir vs workspace root).
/// Canonicalizes when the file exists so the resolver always gets an absolute path.
pub fn workspace_fnox_path() -> PathBuf {
    let path = workspace_root().join("fnox.toml");
    path.canonicalize().unwrap_or(path)
}

/// True if workspace fnox.toml resolves `OPENROUTER_API_KEY` for the default profile.
/// Use to skip LLM integration tests when fnox has no key (e.g. CI without secrets).
pub fn fnox_has_openrouter_key() -> bool {
    use baml_rt_llm_config::{FnoxFileSecretResolver, SecretResolver};
    let resolver = FnoxFileSecretResolver::from_path(Some(workspace_fnox_path().as_path()));
    resolver
        .resolve("OPENROUTER_API_KEY")
        .is_some_and(|v| !v.as_str().trim().is_empty())
}

pub fn setup_baml_runtime(schema_path: &str) -> Arc<Mutex<BamlRuntimeManager>> {
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("Should create manager");
    manager
        .load_schema(schema_path)
        .expect("Should load schema");
    Arc::new(Mutex::new(manager))
}

pub fn setup_baml_runtime_manager(schema_path: &str) -> BamlRuntimeManager {
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("Should create manager");
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

/// Build a runtime manager without an LLM resolver — for unit/integration tests
/// that do NOT make real LLM calls (tool registration, JS bridge tests, etc.).
/// Always prefer this over `BamlRuntimeManager::builder().build()` at call sites
/// so the intent is explicit and traceable.
pub fn setup_baml_runtime_manager_no_llm() -> BamlRuntimeManager {
    BamlRuntimeManager::builder()
        .build()
        .expect("BamlRuntimeManager::builder().build() must succeed")
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

/// QuickJS config for tests. Must accommodate combined retries (parse + BAML client) which can exceed 15s.
fn quickjs_config_for_tests() -> QuickJSConfig {
    QuickJSConfig::new().with_max_attempts_ms(Some(45_000))
}

pub async fn setup_bridge(baml_manager: Arc<Mutex<BamlRuntimeManager>>) -> QuickJSBridge {
    use baml_rt_core::{
        bus::{BusWithEffects, EffectEmitter, EffectLiveness},
        ids::AgentId,
    };
    use uuid::Uuid;
    // Generate a temporary agent_id for test context
    let temp_agent_id = AgentId::from_uuid(baml_rt_core::ids::UuidId::new(Uuid::new_v4()));
    let config = quickjs_config_for_tests();
    // Keep effect emission/liveness wiring aligned with runtime builder semantics.
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut manager = baml_manager.lock().await;
        manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let mut bridge = QuickJSBridge::new_with_config(baml_manager, temp_agent_id, config)
        .await
        .expect("Create QuickJS bridge");
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("Register BAML functions");
    bridge
}

/// Require that OPENROUTER_API_KEY is resolvable from the workspace fnox.toml.
/// Panics with a clear message if not set.
///
/// Local dev: uncomment and fill `default = "sk-or-v1-..."` in fnox.toml.
/// CI: the "Write fnox secrets" workflow step generates fnox.toml with the key before tests run.
pub fn require_api_key() -> String {
    use baml_rt_llm_config::{FnoxFileSecretResolver, SecretResolver};
    let resolver = FnoxFileSecretResolver::from_path(Some(workspace_fnox_path().as_path()));
    resolver
        .resolve("OPENROUTER_API_KEY")
        .map(|v| v.into_string())
        .filter(|s| !s.is_empty())
        .expect(
            "OPENROUTER_API_KEY must be set in fnox.toml \
             (local: uncomment and fill `default` in fnox.toml; CI: Write fnox secrets step)",
        )
}

pub fn ensure_baml_src_exists() -> bool {
    let baml_src = workspace_root().join("baml_src");
    if !baml_src.exists() {
        println!("Skipping test: baml_src directory not found");
        return false;
    }
    true
}

/// Workspace root (repo root). Canonicalizes so paths are absolute and work regardless of test cwd.
/// test-support lives at crates/test-support, so we go up two levels from CARGO_MANIFEST_DIR.
pub fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest_abs = manifest.canonicalize().unwrap_or(manifest);
    manifest_abs
        .parent()
        .and_then(|p| p.parent())
        .expect("test-support crate should be under crates/")
        .to_path_buf()
}

/// Removes a temporary directory tree on drop.
#[derive(Debug)]
pub struct TempDirCleanup {
    path: PathBuf,
}

impl TempDirCleanup {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

/// Temporarily sets an environment variable for the lifetime of this guard.
///
/// Restores the previous value on drop.
#[derive(Debug)]
pub struct TempEnvVar {
    key: String,
    previous: Option<String>,
}

impl TempEnvVar {
    pub fn set(key: &str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: test helper used in controlled test code; callers must avoid
        // concurrent mutation of the same env key from multiple threads.
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }

    pub fn remove(key: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: test helper used in controlled test code; callers must avoid
        // concurrent mutation of the same env key from multiple threads.
        unsafe {
            std::env::remove_var(key);
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => {
                // SAFETY: mirrors set/remove safety note above.
                unsafe {
                    std::env::set_var(&self.key, value);
                }
            }
            None => {
                // SAFETY: mirrors set/remove safety note above.
                unsafe {
                    std::env::remove_var(&self.key);
                }
            }
        }
    }
}

/// Asserts that a tool is visible in QuickJS (either as a JS tool in `__js_tools` or as a Rust tool via `openToolSession`).
/// Takes `&mut QuickJSBridge` for test ergonomics — only use from single-context test harnesses.
pub async fn assert_tool_registered_in_js(
    bridge: &mut QuickJSBridge,
    tool_name: &str,
    scope: &baml_rt_core::context::InvocationScope,
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
                const session = await openToolSession("{}");
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
    let result = bridge
        .eval_scoped(scope, &js_code)
        .await
        .unwrap_or_else(|e| {
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

/// In-memory GraphQLite store for tests that build A2aAgent (persistent mode required).
pub fn test_graphqlite_store() -> std::sync::Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("in-memory provenance store for test")
}

/// Builds a minimal A2aAgent for malformed/error-path A2A tests: no BAML schema or tools.
/// Uses BusWithEffects and QuickJSConfig with max_attempts_ms(15_000).
pub async fn build_minimal_a2a_agent(init_js: &str) -> A2aAgent {
    A2aAgent::builder()
        .with_init_js(init_js)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_graphqlite_store(test_graphqlite_store())
        .build()
        .await
        .expect("build minimal a2a agent")
}

/// Same as build_minimal_a2a_agent but with a short stream collector idle (secs) so tests finish quickly.
pub async fn build_minimal_a2a_agent_with_stream_idle_secs(
    init_js: &str,
    stream_idle_secs: u64,
) -> A2aAgent {
    A2aAgent::builder()
        .with_init_js(init_js)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(
            QuickJSConfig::new()
                .with_max_attempts_ms(Some(15_000))
                .with_stream_collector_idle_secs(Some(stream_idle_secs)),
        )
        .build()
        .await
        .expect("build minimal a2a agent")
}

/// Builds an A2aAgent for contract tests: stream-baml-tool fixture, CalculatorTool,
/// test QuickJS config. Call `ensure_fixture_runtime_types()` before this if not already done.
/// Returns the agent; tests create scope via `InvocationScope::synthetic_message(agent.agent_id().clone())`.
pub async fn setup_stream_baml_tool_agent_for_contract(init_js: Option<&str>) -> A2aAgent {
    let agent_dir = agent_fixture("stream-baml-tool");
    let mut baml_manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    baml_manager
        .load_schema(agent_dir.to_str().expect("fixture path valid"))
        .expect("load schema");
    baml_manager
        .register_tool(CalculatorTool)
        .await
        .expect("register CalculatorTool");
    let config = QuickJSConfig::new().with_max_attempts_ms(Some(45_000));
    let mut builder = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(config)
        .with_graphqlite_store(test_graphqlite_store());
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

/// BAML function name used in `stream-baml-tool` E2E session plan tests.
pub const STREAM_BAML_TOOL_FUNCTION: &str = "ChooseCalcTool";

/// Drives a strict Open→Send→Next→Finish calculator session plan end-to-end.
///
/// Accepts either a raw BAML result (containing `"step": { "op": "Send", ... }`) or
/// an already-sent status (`"status": "sent"`). After confirming `sent`, calls `Next`
/// then `Finish` and returns the numeric result extracted from `output.result`.
pub async fn execute_calc_session_strict(
    manager: &BamlRuntimeManager,
    scope: &baml_rt_core::context::InvocationScope,
    tool_choice: serde_json::Value,
) -> baml_rt_core::Result<f64> {
    use baml_rt_core::BamlRtError;
    let initial_status = tool_choice.get("status").and_then(|v| v.as_str());
    if initial_status != Some("sent") {
        let has_step = tool_choice
            .get("step")
            .and_then(|v| v.as_object())
            .is_some();
        if !has_step {
            return Err(BamlRtError::InvalidArgument(format!(
                "expected strict single-step plan or sent status, got: {tool_choice}"
            )));
        }
        let sent = manager
            .execute_tool_from_baml_result_or_value(
                scope.as_scope(),
                tool_choice,
                Some(STREAM_BAML_TOOL_FUNCTION),
                None,
            )
            .await?;
        if sent.get("status").and_then(|v| v.as_str()) != Some("sent") {
            return Err(BamlRtError::InvalidArgument(format!(
                "expected sent status, got {sent}"
            )));
        }
    }

    // Pass an empty input object so the tool_fsm merges nothing into the previously-sent
    // expression; a non-empty expression here would override the Send payload (e.g. 0+0=0).
    let next = manager
        .execute_tool_from_baml_result_or_value(
            scope.as_scope(),
            serde_json::json!({ "step": { "op": "Read", "input": {} } }),
            Some(STREAM_BAML_TOOL_FUNCTION),
            None,
        )
        .await?;
    if next.get("status").and_then(|v| v.as_str()) != Some("done") {
        return Err(BamlRtError::InvalidArgument(format!(
            "expected done status, got {next}"
        )));
    }
    let value = next
        .get("output")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "missing output.result in strict Next response: {next}"
            ))
        })?;

    let finished = manager
        .execute_tool_from_baml_result_or_value(
            scope.as_scope(),
            serde_json::json!({ "step": { "op": "Finish" } }),
            Some(STREAM_BAML_TOOL_FUNCTION),
            None,
        )
        .await?;
    if finished.get("status").and_then(|v| v.as_str()) != Some("finished") {
        return Err(BamlRtError::InvalidArgument(format!(
            "expected finished status, got {finished}"
        )));
    }
    Ok(value)
}
