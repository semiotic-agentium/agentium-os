//! Linear typestate builder for runner construction.
//!
//! Phases: Loading (load agents) -> Ready (serve/invoke/stdio).
//! Execution entrypoints are only available on RunnerReady, guaranteeing
//! discovery and A2A providers are wired before any mode runs.

use std::{path::Path, sync::Arc};

use baml_rt_a2a::A2aRequestHandler;
use baml_rt_core::{AgentLister, Result};
use baml_rt_provenance::ToolIndexConfig;
use serde_json::Value;

use crate::{AgentPackage, BootedAgent, ProvenanceConfig, RunnerRegistry, ToolAccessPolicy};

/// Builder state: loading agent packages. No execution entrypoints yet.
pub struct Loading;

/// Terminal state: all dependencies wired, execution entrypoints available.
pub struct Ready;

/// Linear builder for the agent runner. Progression: Loading -> Ready.
pub struct RunnerBuilder<S> {
    pub(crate) runner: Arc<crate::AgentRunner>,
    pub(crate) registry: Arc<RunnerRegistry>,
    _state: std::marker::PhantomData<S>,
}

impl RunnerBuilder<Loading> {
    /// Start building: parse config and create empty runner + registry.
    /// Registry is wired to the runner so discovery/A2A see agents as they are loaded.
    pub fn new(
        provenance_config: ProvenanceConfig,
        tool_index: Option<ToolIndexConfig>,
        access_policy: ToolAccessPolicy,
    ) -> Self {
        let runner = Arc::new(crate::AgentRunner::new(
            provenance_config,
            tool_index,
            access_policy,
        ));
        let registry = Arc::new(RunnerRegistry(Arc::clone(&runner)));
        Self {
            runner,
            registry,
            _state: std::marker::PhantomData,
        }
    }

    /// Load one agent package. Registry (discover_agents, internal_a2a) sees agents already loaded.
    pub async fn load_agent(self, package_path: &Path) -> Result<RunnerBuilder<Loading>> {
        let package = AgentPackage::load_from_file(package_path).await?;
        let name = package.name().to_string();
        let catalogue = self.registry.clone() as Arc<dyn AgentLister>;
        let a2a_handler = self.registry.clone() as Arc<dyn A2aRequestHandler>;
        let (agent, _agent_id) = package
            .boot(
                self.runner.provenance_config(),
                self.runner.tool_index().clone(),
                self.runner.access_policy(),
                catalogue,
                a2a_handler,
            )
            .await?;
        let manifest = package.manifest().clone();
        let booted = BootedAgent {
            agent,
            manifest: manifest.clone(),
        };
        tracing::info!(agent = %name, "Agent loaded and booted successfully");
        self.runner.insert_agent(name.clone(), booted);
        Ok(self)
    }

    /// Finish loading and transition to Ready. No more agents can be added; execution entrypoints unlock.
    pub fn build(self) -> RunnerBuilder<Ready> {
        RunnerBuilder {
            runner: self.runner,
            registry: self.registry,
            _state: std::marker::PhantomData,
        }
    }
}

impl RunnerBuilder<Ready> {
    /// List loaded agent names (CLI display).
    /// Invariant: only available after build(); discovery is wired before any execution mode runs.
    pub fn list_agents(&self) -> Vec<String> {
        self.runner.list_agents()
    }

    /// Invoke a function on an agent (invoke mode).
    pub async fn invoke(
        &self,
        agent_name: &str,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        self.runner.invoke(agent_name, function_name, args).await
    }

    /// Run A2A over stdin/stdout (stdio mode).
    pub async fn run_a2a_stdio(&self) -> Result<()> {
        self.runner.run_a2a_stdio().await
    }

    /// Registry for HTTP serve (implements AgentRegistry, AgentLister, A2aRequestHandler).
    pub fn registry(&self) -> Arc<RunnerRegistry> {
        Arc::clone(&self.registry)
    }

    /// Runner arc (for provenance, mermaid, discovery, or custom A2A loop).
    pub fn runner(&self) -> Arc<crate::AgentRunner> {
        Arc::clone(&self.runner)
    }
}
