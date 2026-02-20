use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
#[cfg(any(feature = "clickup", feature = "notion"))]
use baml_rt_a2a::AgentRegistry;
#[cfg(any(feature = "clickup", feature = "notion"))]
use baml_rt_core::A2aRequestHandler;
use baml_rt_core::ids::ContextId;
#[cfg(any(feature = "clickup", feature = "notion"))]
use baml_rt_core::{AgentCard, AgentDiscoveryEntry, AgentLister, AgentRouteKey};
#[cfg(any(feature = "clickup", feature = "notion"))]
use baml_rt_provenance::{
    GraphExporter,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
};
use baml_rt_provenance::{
    GraphqliteProvenanceStore, ProvEvent, ProvenanceContextMessage, ProvenanceContextReader,
    ProvenanceConversationContextItem, ProvenanceWriter,
};
#[cfg(any(feature = "clickup", feature = "notion"))]
use serde_json::Value;
#[cfg(any(feature = "clickup", feature = "notion"))]
pub use test_support::common::TempEnvVar;
use tokio::sync::Semaphore;

pub fn init_test_tracing() {
    static TRACING: OnceLock<()> = OnceLock::new();
    TRACING.get_or_init(|| {
        baml_rt_observability::init_tracing();
    });
}

pub fn e2e_serial_gate() -> &'static Semaphore {
    init_test_tracing();
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    GATE.get_or_init(|| Semaphore::new(1))
}

#[derive(Clone)]
pub struct StrictProvenanceWriter {
    inner: Arc<GraphqliteProvenanceStore>,
}

impl StrictProvenanceWriter {
    pub fn new(inner: Arc<GraphqliteProvenanceStore>) -> Self {
        Self { inner }
    }
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

#[cfg(any(feature = "clickup", feature = "notion"))]
#[derive(Clone)]
pub struct SingleAgentRegistry {
    package: String,
    instance_id: String,
    name: String,
    version: String,
    agent: baml_rt::A2aAgent,
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl SingleAgentRegistry {
    pub fn new(
        package: &str,
        instance_id: &str,
        name: &str,
        version: &str,
        agent: baml_rt::A2aAgent,
    ) -> Self {
        Self {
            package: package.to_string(),
            instance_id: instance_id.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            agent,
        }
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
#[async_trait]
impl AgentLister for SingleAgentRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let agent_card = AgentCard {
            name: self.name.clone(),
            version: self.version.clone(),
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            tools: Vec::new(),
            description: None,
            capabilities: Vec::new(),
        };
        vec![AgentDiscoveryEntry {
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            agent_card,
        }]
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
#[async_trait]
impl AgentRegistry for SingleAgentRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<Value>> {
        if key.agent_package != self.package || key.agent_instance_id != self.instance_id {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {}/{} not found",
                key.agent_package, key.agent_instance_id
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
pub struct TestMermaidService {
    store: Arc<GraphqliteProvenanceStore>,
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl TestMermaidService {
    pub fn new(store: Arc<GraphqliteProvenanceStore>) -> Self {
        Self { store }
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
#[async_trait]
impl baml_rt_api::MermaidService for TestMermaidService {
    async fn mermaid_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_context(context_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
    }

    async fn mermaid_for_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_task(task_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
pub struct RunningHttpServer {
    pub base_url: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl RunningHttpServer {
    fn new(
        base_url: String,
        shutdown_tx: tokio::sync::oneshot::Sender<()>,
        handle: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            base_url,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    pub fn with_base_path(mut self, base_path: &str) -> Self {
        let trimmed = base_path.trim();
        if trimmed.is_empty() || trimmed == "/" {
            return self;
        }
        if trimmed.starts_with('/') {
            self.base_url.push_str(trimmed);
        } else {
            self.base_url.push('/');
            self.base_url.push_str(trimmed);
        }
        self
    }

    pub async fn stop(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl Drop for RunningHttpServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
#[derive(Debug)]
pub struct TempDirCleanup {
    path: PathBuf,
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl TempDirCleanup {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

#[cfg(any(feature = "clickup", feature = "notion"))]
pub async fn start_http_server(app: axum::Router) -> std::io::Result<RunningHttpServer> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(RunningHttpServer::new(
        format!("http://{addr}"),
        shutdown_tx,
        handle,
    ))
}

#[cfg(any(feature = "clickup", feature = "notion"))]
pub async fn start_runner_api_server(
    agent_package: &str,
    agent: baml_rt::A2aAgent,
    provenance: Arc<GraphqliteProvenanceStore>,
) -> std::io::Result<RunningHttpServer> {
    let registry: Arc<dyn AgentRegistry> = Arc::new(SingleAgentRegistry::new(
        agent_package,
        "default",
        agent_package,
        "1.0.0",
        agent,
    ));
    let mermaid: Option<Arc<dyn baml_rt_api::MermaidService>> =
        Some(Arc::new(TestMermaidService::new(provenance)));
    let app = baml_rt_api::api_router(registry, mermaid, None);
    start_http_server(app).await
}

#[cfg(any(feature = "clickup", feature = "notion"))]
pub fn contains_kv(value: &Value, key: &str, expected: &str) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| {
            (k == key && v.as_str() == Some(expected)) || contains_kv(v, key, expected)
        }),
        Value::Array(items) => items.iter().any(|v| contains_kv(v, key, expected)),
        _ => false,
    }
}

pub fn build_agent_dir_to_temp(
    agent_dir: &Path,
    package_label: &str,
    builder_features: Option<&str>,
) -> PathBuf {
    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!("Agent directory {} missing or invalid", agent_dir.display());
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tar_path =
        std::env::temp_dir().join(format!("runner-test-{package_label}-{unique}.tar.gz"));
    let extract_dir =
        std::env::temp_dir().join(format!("runner-test-{package_label}-extract-{unique}"));
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).expect("create extract dir");

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(test_support::common::workspace_root())
        .arg("run")
        .arg("--quiet")
        .arg("-p")
        .arg("baml-rt-builder");
    if let Some(features) = builder_features {
        cmd.arg("--features").arg(features);
    }
    cmd.arg("--bin")
        .arg("baml-agent-builder")
        .arg("--")
        .arg("package")
        .arg("--agent-dir")
        .arg(agent_dir)
        .arg("--output")
        .arg(&tar_path)
        .arg("--skip-lint");

    let output = cmd.output().expect("build agent: run builder");
    if !output.status.success() {
        panic!(
            "build agent {} failed: stdout={}, stderr={}",
            package_label,
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

#[cfg(feature = "clickup")]
#[allow(dead_code)] // Helper is consumed by clickup integration tests when compiled as their own test target.
pub async fn build_clickup_agent_to_temp_async() -> PathBuf {
    let clickup_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("clickup-agent");
    tokio::task::spawn_blocking(move || {
        build_agent_dir_to_temp(&clickup_agent_dir, "clickup-agent", Some("clickup"))
    })
    .await
    .expect("build clickup agent task join")
}

#[cfg(feature = "notion")]
#[allow(dead_code)] // Helper is consumed by notion integration tests when compiled as their own test target.
pub async fn build_notion_agent_to_temp_async() -> PathBuf {
    let notion_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("notion-agent");
    tokio::task::spawn_blocking(move || {
        build_agent_dir_to_temp(&notion_agent_dir, "notion-agent", Some("notion"))
    })
    .await
    .expect("build notion agent task join")
}
