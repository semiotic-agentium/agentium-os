//! Common test utilities and shared modules.

pub use crate::support::tools::*;
mod a2a_test_helpers;
pub use a2a_test_helpers::{
    await_first_match, chunk_content, chunks_from_responses, first_message_text_from_stream,
    first_task_id_from_stream, is_error_response, message_texts_from_chunks,
    message_visible_content_from_chunks, send_stream_request, send_stream_request_with_task,
    user_message, user_message_with_task,
};
mod net;
pub use net::{bind_ephemeral_tokio, reserve_ephemeral_addr};
mod test_tools;
// Fixture helpers
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use baml_rt::{A2aAgent, QuickJSConfig, baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge};
use baml_rt_core::bus::{
    BusWithEffects, EffectEmitter, EffectEvent, EffectLiveness, EffectSubscriber,
    EffectSubscriberTier,
};
use baml_rt_provenance::SurrealStoreBuilder;
pub use test_tools::{
    AddNumbersInput, AddNumbersOutput, AddNumbersTool, DelayedResponseTool, UppercaseTool,
    WeatherTool,
};
use tokio::sync::RwLock;

/// Deep-copy an agent fixture tree into `dst` so the builder can write tsconfig / generated `.d.ts`
/// without touching the committed workspace (read-only checkouts, Nix sandboxes, sparse clones).
///
/// Exposed for tests that need to clone a fixture into a temp dir and mutate it
/// before building (e.g. content-hash perturbation tests in the cluster-agents
/// integration suite).
pub fn copy_agent_tree_for_build(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("agent source is not a directory: {}", src.display()),
        ));
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_agent_tree_for_build(&path, &dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

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
    fn name(&self) -> &'static str {
        "test_capturing"
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        self.events.lock().await.push(event.clone());
        Ok(())
    }

    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Awaitable
    }
}

/// Creates a `QuickJSBridge` wired with a `BusWithEffects` and a `CapturingEffectSubscriber`.
///
/// Returns `(bridge, capture)` — the bridge has BAML functions registered and effect
/// liveness set. The capture accumulates all emitted `EffectEvent`s for later assertion.
pub async fn make_capturing_bridge(
    agent_id: baml_rt_core::ids::AgentId,
) -> (QuickJSBridge, Arc<CapturingEffectSubscriber>) {
    let manager = Arc::new(RwLock::new(
        BamlRuntimeManager::new().expect("create BamlRuntimeManager"),
    ));
    let effect_bus = Arc::new(BusWithEffects::new());
    let capture = Arc::new(CapturingEffectSubscriber::default());
    effect_bus.subscribe_effect(capture.clone()).await;
    {
        let mut guard = manager.write().await;
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

    let staging = std::env::temp_dir().join(format!(
        "a2a-test-{fixture_name}-stage-{pid}-{unique}",
        fixture_name = fixture_name,
        pid = pid,
        unique = unique
    ));
    let _ = fs::remove_dir_all(&staging);
    copy_agent_tree_for_build(&agent_dir, &staging).unwrap_or_else(|e| {
        panic!(
            "copy fixture {fixture_name} from {} to staging {} failed: {e}",
            agent_dir.display(),
            staging.display()
        );
    });

    baml_rt_builder::build_agent_package(&staging, &tar_path)
        .await
        .unwrap_or_else(|e| panic!("build fixture {fixture_name} failed: {e}"));
    let _ = fs::remove_dir_all(&staging);

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

    let staging = std::env::temp_dir().join(format!(
        "runner-test-{package_label}-stage-{pid}-{unique}",
        package_label = package_label,
        pid = pid,
        unique = unique
    ));
    let _ = fs::remove_dir_all(&staging);
    copy_agent_tree_for_build(&agent_dir, &staging).unwrap_or_else(|e| {
        panic!(
            "copy agent {package_label} from {} to staging {} failed: {e}",
            agent_dir.display(),
            staging.display()
        );
    });

    baml_rt_builder::build_agent_package(&staging, &tar_path)
        .await
        .unwrap_or_else(|e| panic!("build agent {package_label} failed: {e}"));
    let _ = fs::remove_dir_all(&staging);

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

/// Builds an agent archive at the given path using the builder crate (no subprocess) and returns
/// the `.tar.gz` path. Caller is responsible for removing the archive when done.
pub async fn build_agent_package_archive_to_temp(
    agent_dir: PathBuf,
    package_label: &str,
) -> PathBuf {
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

    let staging = std::env::temp_dir().join(format!(
        "runner-test-{package_label}-archive-stage-{pid}-{unique}",
        package_label = package_label,
        pid = pid,
        unique = unique
    ));
    let _ = fs::remove_dir_all(&staging);
    copy_agent_tree_for_build(&agent_dir, &staging).unwrap_or_else(|e| {
        panic!(
            "copy agent archive {package_label} from {} to staging {} failed: {e}",
            agent_dir.display(),
            staging.display()
        );
    });

    baml_rt_builder::build_agent_package(&staging, &tar_path)
        .await
        .unwrap_or_else(|e| panic!("build agent archive {package_label} failed: {e}"));
    let _ = fs::remove_dir_all(&staging);

    tar_path
}

/// Assert that fixture TypeScript runtime declarations exist.
///
/// Scans `tests/fixtures/agents/` for directories containing `baml_src/`
/// and asserts each also has `src/baml-runtime.d.ts`. These files are
/// committed; refresh with `just regen-fixtures` (or
/// `cargo run -p baml-rt-builder --all-features --bin regen_fixtures`).
/// When using pre-commit, the `regen-fixtures` hook re-runs regen when
/// relevant paths are staged (see `.pre-commit-config.yaml`).
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
             Run: cargo run -p baml-rt-builder --all-features --bin regen_fixtures\n\
             (or `just regen-fixtures`)"
        );
    }
}

/// Path to workspace-root fnox.toml. Use with
/// `BamlRuntimeManager::builder().with_fnox_llm_resolver(workspace_fnox_path())` so resolution
/// works regardless of test cwd (package dir vs workspace root).
/// Canonicalizes when the file exists so the resolver always gets an absolute path.
///
/// **First call** loads `.env` (if present) into the process environment so fnox secret
/// resolution can substitute `env.*` / `$VAR` placeholders — same as a shell that ran `source .env`
/// before `cargo test` / nextest. In git worktrees we also probe the shared repo-root `.env`.
pub fn workspace_fnox_path() -> PathBuf {
    load_workspace_dotenv_once();
    let path = workspace_root().join("fnox.toml");
    path.canonicalize().unwrap_or(path)
}

fn shared_repo_root() -> Option<PathBuf> {
    let git_path = workspace_root().join(".git");
    if git_path.is_dir() {
        return Some(workspace_root());
    }
    if !git_path.is_file() {
        return None;
    }

    let raw = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = raw.strip_prefix("gitdir: ")?.trim();
    let gitdir_path = {
        let candidate = PathBuf::from(gitdir);
        if candidate.is_absolute() {
            candidate
        } else {
            workspace_root().join(candidate)
        }
    }
    .canonicalize()
    .ok()?;

    gitdir_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
}

fn load_workspace_dotenv_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let workspace_dotenv = workspace_root().join(".env");
        let shared_dotenv = shared_repo_root()
            .map(|root| root.join(".env"))
            .filter(|path| path != &workspace_dotenv);

        for dotenv_path in [Some(workspace_dotenv), shared_dotenv]
            .into_iter()
            .flatten()
        {
            if dotenv_path.is_file()
                && let Err(e) = dotenvy::from_path(&dotenv_path)
            {
                tracing::debug!(
                    path = %dotenv_path.display(),
                    error = %e,
                    "test-support: optional workspace .env load failed"
                );
            }
        }
    });
}

/// True if `OPENROUTER_API_KEY` is available from the process environment or workspace
/// `fnox.toml` (after loading workspace `.env` once — see [`workspace_fnox_path`]).
pub fn fnox_has_openrouter_key() -> bool {
    use baml_rt_llm_config::{FnoxFileSecretResolver, SecretResolver};
    let fnox_path = workspace_fnox_path();
    if std::env::var("OPENROUTER_API_KEY")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
    {
        return true;
    }
    let resolver = FnoxFileSecretResolver::from_path(Some(fnox_path.as_path()));
    resolver
        .resolve("OPENROUTER_API_KEY")
        .is_some_and(|v| !v.as_str().trim().is_empty())
}

/// True if `CLICKUP_API_KEY` is available from the process environment or workspace `fnox.toml`
/// (after [`workspace_fnox_path`] loads `.env` once).
pub fn fnox_has_clickup_key() -> bool {
    use baml_rt_llm_config::{FnoxFileSecretResolver, SecretResolver};
    if std::env::var("CLICKUP_API_KEY")
        .ok()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    let resolver = FnoxFileSecretResolver::from_path(Some(workspace_fnox_path().as_path()));
    for key in ["env.CLICKUP_API_KEY", "CLICKUP_API_KEY"] {
        if resolver
            .resolve(key)
            .is_some_and(|v| !v.as_str().trim().is_empty())
        {
            return true;
        }
    }
    false
}

pub fn setup_baml_runtime(schema_path: &str) -> Arc<RwLock<BamlRuntimeManager>> {
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("Should create manager");
    manager
        .load_schema(schema_path)
        .expect("Should load schema");
    Arc::new(RwLock::new(manager))
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

pub fn setup_baml_runtime_default() -> Arc<RwLock<BamlRuntimeManager>> {
    setup_baml_runtime(
        workspace_root()
            .join("baml_src")
            .to_str()
            .expect("Workspace baml_src path should be valid"),
    )
}

pub fn setup_baml_runtime_from_fixture(fixture_name: &str) -> Arc<RwLock<BamlRuntimeManager>> {
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

pub async fn setup_bridge(baml_manager: Arc<RwLock<BamlRuntimeManager>>) -> QuickJSBridge {
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
        let mut manager = baml_manager.write().await;
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

/// Require that `OPENROUTER_API_KEY` resolves either via workspace `fnox.toml` or directly from
/// the process environment after loading workspace `.env` once (see [`workspace_fnox_path`]).
///
/// Local dev: set secrets in `fnox.toml` (often with `env.OPENROUTER_API_KEY` pointing at vars from
/// `.env`). CI: workflow writes fnox secrets.
pub fn require_api_key() -> String {
    use baml_rt_llm_config::{FnoxFileSecretResolver, SecretResolver};
    let fnox_path = workspace_fnox_path();
    let resolver = FnoxFileSecretResolver::from_path(Some(fnox_path.as_path()));
    resolver
        .resolve("OPENROUTER_API_KEY")
        .map(|v| v.into_string())
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
        .filter(|s| !s.is_empty())
        .expect(
            "OPENROUTER_API_KEY must be set via fnox.toml or environment \
             (local: configure fnox and/or `.env`; CI: Write fnox secrets step)",
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

/// In-memory SurrealDB store for tests that build A2aAgent (persistent mode required).
///
/// Uses **isolated** memory (not [`SurrealStoreBuilder::in_memory`], which is a process-wide singleton).
/// A shared store survives across tests; tearing down streams/agents can close internal channels and
/// surface `Failed to read provenance context` / `sending into a closed channel` in later tests.
pub async fn test_surreal_store() -> std::sync::Arc<baml_rt_provenance::SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory isolated provenance store for test")
}

/// Builds a minimal A2aAgent for malformed/error-path A2A tests: no BAML schema or tools.
/// Uses BusWithEffects and QuickJSConfig with max_attempts_ms(15_000).
pub async fn build_minimal_a2a_agent(init_js: &str) -> A2aAgent {
    A2aAgent::builder()
        .with_init_js(init_js)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_surreal_store(test_surreal_store().await)
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
        .with_surreal_store(test_surreal_store().await);
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

/// Extracts `result.result` from a blocking Send completion payload (`send_done_json`).
fn calc_numeric_result_from_tool_payload(val: &serde_json::Value) -> Option<f64> {
    val.get("result")?.get("result")?.as_f64()
}

/// Drives a strict calculator session plan end-to-end.
///
/// Accepts a raw BAML result (`step`: Open / Send / …) or an executor payload that already
/// completed blocking Send (`status`: `done` with typed `result`). Host Send blocks until the
/// tool returns [`ToolStep::Done`] and archives output; there is no separate legacy `sent` +
/// empty `PageRead` hop for calculator fixtures.
pub async fn execute_calc_session_strict(
    manager: &BamlRuntimeManager,
    scope: &baml_rt_core::context::InvocationScope,
    tool_choice: serde_json::Value,
) -> baml_rt_core::Result<f64> {
    use baml_rt_core::BamlRtError;

    if let Some(v) = calc_numeric_result_from_tool_payload(&tool_choice) {
        return Ok(v);
    }

    let initial_status = tool_choice.get("status").and_then(|v| v.as_str());
    let has_step = tool_choice
        .get("step")
        .and_then(|v| v.as_object())
        .is_some();

    if !has_step {
        return Err(BamlRtError::InvalidArgument(format!(
            "expected session plan step or done payload with calculator result, got: {tool_choice}"
        )));
    }

    if initial_status == Some("sent") {
        return Err(BamlRtError::InvalidArgument(
            "legacy intermediate status \"sent\" is not supported for strict calculator E2E; Send blocks until done"
                .to_string(),
        ));
    }

    let executed = manager
        .execute_tool_from_baml_result_or_value(
            scope.as_scope(),
            tool_choice,
            Some(STREAM_BAML_TOOL_FUNCTION),
            None,
        )
        .await?;

    calc_numeric_result_from_tool_payload(&executed).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "expected blocking Send completion with calculator result.result, got {executed}"
        ))
    })
}
