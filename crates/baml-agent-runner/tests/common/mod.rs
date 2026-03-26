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
    AgentDispatchRequest, AgentLister, AgentRouteKey,
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
#[derive(Clone)]
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
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {}/{} not found",
                key.agent_package.as_str(),
                key.agent_instance_id.as_str()
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        _request: AgentDispatchRequest,
    ) -> baml_rt_core::Result<AgentDispatchAck> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {}/{} not found",
                key.agent_package.as_str(),
                key.agent_instance_id.as_str()
            )));
        }
        Err(baml_rt_core::BamlRtError::FunctionNotFound(
            "onDispatch".to_string(),
        ))
    }
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
pub struct RunningHttpServer {
    pub base_url: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
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
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
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

#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
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

/// POST to /a2a/sse and collect all JSON-RPC responses from the SSE stream.
#[cfg(any(
    feature = "clickup",
    feature = "notion",
    feature = "slack",
    feature = "llm-tests"
))]
pub async fn post_a2a_sse_collect(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let mut response = client
        .post(url)
        .header("Accept", "text/event-stream")
        .json(body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, text).into());
    }
    let mut responses = Vec::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline_idx) = buffer.find('\n') {
            let line = buffer[..newline_idx].trim().to_string();
            buffer.drain(..=newline_idx);
            if !line.starts_with("data:") {
                continue;
            }
            let json_str = line.strip_prefix("data:").unwrap_or(&line).trim();
            if json_str.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(json_str) {
                let is_final = v
                    .get("result")
                    .and_then(|result| result.get("final"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                responses.push(v);
                if is_final {
                    return Ok(responses);
                }
            }
        }
    }

    let trailing = buffer.trim();
    if trailing.starts_with("data:") {
        let json_str = trailing.strip_prefix("data:").unwrap_or(trailing).trim();
        if !json_str.is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(json_str)
        {
            responses.push(v);
        }
    }
    Ok(responses)
}
