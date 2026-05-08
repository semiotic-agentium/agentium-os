//! Shared helpers for HTTP / e2e tests. Many items are optional per integration test binary.
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use std::sync::Arc;
use std::{path::PathBuf, sync::OnceLock};

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use baml_rt_a2a::AgentRegistry;
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use baml_rt_core::A2aRequestHandler;
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentLister, AgentRouteKey, event_subscription::EventSubscription,
};
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use baml_rt_provenance::{
    GraphExporter, SurrealProvenanceStore,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
};
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
use serde_json::Value;
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(unused_imports)] // Used by clickup/notion/slack test binaries, not all.
pub use test_support::common::{TempDirCleanup, TempEnvVar};
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

/// Seconds to use in CI (`CI` set) vs local runs — for stream idle, wall-clock envelopes, etc.
///
/// Not every `tests/*.rs` binary links all helpers; integration targets are feature-split.
#[allow(dead_code)]
pub fn e2e_secs_ci_or_local(ci_secs: u64, local_secs: u64) -> u64 {
    if std::env::var_os("CI").is_some() {
        ci_secs
    } else {
        local_secs
    }
}

/// SurrealDB container image tag used by cluster-mode integration tests.
/// Keep in sync with `deploy/helm/agentium-os/values.yaml` so adversarial
/// and routing tests both exercise the same SurrealDB version the pilot
/// ships.
#[allow(dead_code)] // Only cluster-tests binaries reach this constant.
pub const CLUSTER_SURREALDB_IMAGE_TAG: &str = "v3.0.4";

/// RFC 1918 address used as a dummy advertised runner endpoint in
/// cluster-mode tests. Satisfies the SSRF validator's private-range
/// allowance without the test runner actually accepting traffic on that
/// address — the cluster path under test only needs the URL to be
/// validation-clean.
#[allow(dead_code)] // Only cluster-tests binaries reach this constant.
pub const FAKE_CLUSTER_RUNNER_ENDPOINT: &str = "http://10.0.0.1:18080";

/// Load workspace `.env` when present. Missing file is normal (CI); other I/O is surfaced.
///
/// Not every `tests/*.rs` binary links all helpers; integration targets are feature-split.
#[allow(dead_code)]
pub fn try_load_dotenv_for_tests() {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("dotenvy::dotenv: {e}"),
    }
}

/// RAII guard that removes a single temp file on drop.
///
/// Pair with [`test_support::common::build_agent_package_archive_to_temp`] so a built
/// `.tar.gz` is cleaned up even if the test panics.
#[allow(dead_code)] // Not every integration binary publishes packages.
pub struct TempFileCleanup {
    path: std::path::PathBuf,
}

impl TempFileCleanup {
    #[allow(dead_code)]
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Publish a built `.tar.gz` to the runner's embedded repository. Returns the
/// content hash. The `rationale` is recorded on the published entry so each
/// caller can identify which test wrote it.
#[allow(dead_code)] // Not every integration binary publishes packages.
pub async fn publish_fixture(
    client: &reqwest::Client,
    base_url: &str,
    tar_path: &std::path::Path,
    token: &str,
    rationale: &str,
) -> String {
    use std::str::FromStr;

    use baml_rt_repository::{
        commands::{PublishCommand, PublishOrigin, PublishResult},
        entry::ChangeRationale,
        ids::AgentName,
        package::source_bundle_from_tar_gz,
    };

    let bytes = std::fs::read(tar_path).expect("read package tar");
    let (_, source) =
        source_bundle_from_tar_gz(&bytes).expect("parse package as repository source bundle");
    let name_str = source.manifest.name().expect("manifest name in package");
    let cmd = PublishCommand {
        name: AgentName::from_str(name_str).expect("valid AgentName"),
        source,
        rationale: ChangeRationale::new(rationale).expect("non-empty rationale"),
        origin: PublishOrigin::Original,
    };
    let publish_url = format!("{base_url}/repository/publish");
    let resp = client
        .post(&publish_url)
        .header("X-Runner-Token", token)
        .json(&cmd)
        .send()
        .await
        .expect("POST /repository/publish");
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        panic!("publish failed: {text}");
    }
    let result: PublishResult = resp.json().await.expect("PublishResult JSON");
    result.hash.to_string()
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct SingleAgentRegistry {
    package: String,
    instance_id: String,
    name: String,
    version: String,
    agent: baml_rt::A2aAgent,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
impl SingleAgentRegistry {
    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl AgentLister for SingleAgentRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let agent_card = AgentCard {
            name: self.name.clone(),
            version: self.version.clone(),
            content_hash: None,
            repository_version: None,
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            tools: Vec::new(),
            baml_functions: Vec::new(),
            description: None,
            capabilities: Vec::new(),
            tags: Vec::new(),
            subscriptions: Vec::new(),
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl AgentRegistry for SingleAgentRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<A2aStreamChunk>> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> baml_rt_core::Result<AgentDispatchAck> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_dispatch(request).await
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct DispatchRegistry {
    pub package: String,
    pub instance_id: String,
    pub name: String,
    pub version: String,
    pub subscriptions: Vec<EventSubscription>,
    pub agent: baml_rt::A2aAgent,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
impl DispatchRegistry {
    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
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
            subscriptions: Vec::new(),
            agent,
        }
    }

    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
    pub fn with_subscriptions(mut self, subscriptions: Vec<EventSubscription>) -> Self {
        self.subscriptions = subscriptions;
        self
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl AgentLister for DispatchRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let agent_card = AgentCard {
            name: self.name.clone(),
            version: self.version.clone(),
            content_hash: None,
            repository_version: None,
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            tools: Vec::new(),
            baml_functions: Vec::new(),
            description: None,
            capabilities: Vec::new(),
            tags: Vec::new(),
            subscriptions: self.subscriptions.clone(),
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl AgentRegistry for DispatchRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<A2aStreamChunk>> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> baml_rt_core::Result<AgentDispatchAck> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_dispatch(request).await
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct StaticAgentList {
    pub entries: Vec<AgentDiscoveryEntry>,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
impl AgentLister for StaticAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct DelegationCall {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub prompt: String,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone, Default)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct CapturingA2aHandler {
    pub calls: std::sync::Arc<tokio::sync::Mutex<Vec<DelegationCall>>>,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
impl CapturingA2aHandler {
    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
    pub async fn snapshot_calls(&self) -> Vec<DelegationCall> {
        self.calls.lock().await.clone()
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl A2aRequestHandler for CapturingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<A2aStreamChunk>> {
        use serde_json::Value;
        let target_package = request
            .as_ref()
            .pointer("/params/metadata/target/agent_package")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let target_instance = request
            .as_ref()
            .pointer("/params/metadata/target/agent_instance_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let prompt = request
            .as_ref()
            .pointer("/params/message/parts/0/text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        self.calls.lock().await.push(DelegationCall {
            agent_package: target_package.clone(),
            agent_instance_id: target_instance,
            prompt,
        });

        let response = serde_json::json!({
            "result": {
                "message": {
                    "parts": [{"text": format!("delegated to {target_package}")}]
                }
            }
        });

        Ok(Box::pin(futures_util::stream::iter(vec![
            A2aStreamChunk::from(response),
        ])))
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone, Default)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct FailingA2aHandler;

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl A2aRequestHandler for FailingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<A2aStreamChunk>> {
        Err(baml_rt_core::BamlRtError::InvalidArgument(
            "downstream agent unavailable".to_string(),
        ))
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone)]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct StreamingA2aHandler {
    pub chunks: Vec<serde_json::Value>,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
impl A2aRequestHandler for StreamingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<A2aStreamChunk>> {
        Ok(Box::pin(futures_util::stream::iter(
            self.chunks.clone().into_iter().map(A2aStreamChunk::from),
        )))
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub fn discovery_entry(package: &str, capabilities: &[&str]) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: package.to_string(),
        version: "1.0.0".to_string(),
        content_hash: None,
        repository_version: None,
        agent_package: package.to_string(),
        agent_instance_id: "default".to_string(),
        tools: Vec::new(),
        baml_functions: Vec::new(),
        description: Some(format!("{package} test agent")),
        capabilities: capabilities.iter().map(|v| (*v).to_string()).collect(),
        tags: Vec::new(),
        subscriptions: Vec::new(),
    };

    AgentDiscoveryEntry {
        agent_package: package.to_string(),
        agent_instance_id: "default".to_string(),
        name: package.to_string(),
        version: "1.0.0".to_string(),
        agent_card: card,
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct TestMermaidService {
    store: Arc<SurrealProvenanceStore>,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
impl TestMermaidService {
    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[::async_trait::async_trait]
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

#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub struct RunningHttpServer {
    pub base_url: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl RunningHttpServer {
    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
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

    #[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
    pub async fn stop(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

/// Single place for `http://host:port` + optional API root (e.g. `/v1`, `/api/v2`). Canonicalized once.
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
fn http_server_base_url(addr: std::net::SocketAddr, base_path: Option<&str>) -> String {
    let root = format!("http://{addr}");
    let Some(path) = base_path else {
        return root;
    };
    let path = path.trim_matches('/');
    if path.is_empty() {
        return root;
    }
    let root = root.trim_end_matches('/');
    format!("{root}/{path}")
}

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

#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub async fn start_http_server(
    app: axum::Router,
    base_path: Option<&str>,
) -> std::io::Result<RunningHttpServer> {
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
        http_server_base_url(addr, base_path),
        shutdown_tx,
        handle,
    ))
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub async fn start_runner_api_server(
    agent_package: &str,
    agent: baml_rt::A2aAgent,
    provenance: Arc<SurrealProvenanceStore>,
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
    let app = baml_rt_api::api_router(registry, mermaid, None).await;
    start_http_server(app, None).await
}

/// Builds an agent at the given path using the builder crate (in-process, no cargo subprocess).
#[allow(dead_code)] // Used only by optional http-tools integration tests (slack/clickup/notion).
pub async fn build_agent_dir_to_temp_async(agent_dir: PathBuf, package_label: &str) -> PathBuf {
    test_support::common::build_agent_package_to_temp(agent_dir, package_label).await
}

#[cfg(feature = "clickup")]
#[allow(dead_code)] // Helper is consumed by clickup integration tests when compiled as their own test target.
pub async fn build_clickup_agent_to_temp_async() -> PathBuf {
    let clickup_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("clickup-agent");
    build_agent_dir_to_temp_async(clickup_agent_dir, "clickup-agent").await
}

#[cfg(feature = "notion")]
#[allow(dead_code)] // Helper is consumed by notion integration tests when compiled as their own test target.
pub async fn build_notion_agent_to_temp_async() -> PathBuf {
    let notion_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("notion-agent");
    build_agent_dir_to_temp_async(notion_agent_dir, "notion-agent").await
}

#[cfg(feature = "slack")]
#[allow(dead_code)] // Helper is consumed by slack integration tests when compiled as their own test target.
pub async fn build_slack_agent_to_temp_async() -> PathBuf {
    let slack_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("slack-agent");
    build_agent_dir_to_temp_async(slack_agent_dir, "slack-agent").await
}

/// GET `/contexts/{context_id}/mermaid` with a 20s per-request timeout; panics on non-success HTTP.
///
/// Not every `tests/*.rs` binary links all helpers; integration targets are feature-split.
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(dead_code)]
pub async fn fetch_context_mermaid(
    client: &reqwest::Client,
    base_url: &str,
    context_id: &str,
) -> String {
    use tokio::time::{Duration, timeout};
    let url = format!("{base_url}/contexts/{context_id}/mermaid");
    let mermaid_response = timeout(Duration::from_secs(20), client.get(&url).send())
        .await
        .expect("mermaid request timed out")
        .expect("mermaid request failed");
    assert!(
        mermaid_response.status().is_success(),
        "Expected 200 from /contexts/<context_id>/mermaid, got {}",
        mermaid_response.status()
    );
    mermaid_response.text().await.expect("mermaid body")
}

/// POST to `/a2a` and collect all JSON-RPC responses from the SSE (`text/event-stream`) body.
///
/// Not every `tests/*.rs` binary links all helpers; integration targets are feature-split.
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[allow(dead_code)] // Shared optional test helper; not every integration binary uses it.
pub async fn post_a2a_sse_collect(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let request_url = url.replace("/a2a/sse", "/a2a");
    let response = client.post(&request_url).json(body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, text).into());
    }
    let text = response.text().await?;
    baml_rt_core::parse_a2a_sse_json_rpc_chunks(&text)
        .map_err(|e| format!("Invalid A2A SSE response: {e}").into())
}
