// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Linear typestate builder for runner construction.
//!
//! Phases: Loading (construct builder) → Ready (serve/invoke/stdio). Agents are loaded via
//! deploy-by-hash, not positional `tar.gz` paths; this builder only wires the empty runner.
//! Execution entrypoints are only available on RunnerReady.

use std::sync::Arc;

use baml_rt_core::{ConversationHistoryUpdate, Result};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::{
    routing::RunnerRegistry,
    runner::{AgentRunner, AgentRunnerConfig},
};

/// Builder state: runner constructed; add agents via [`AgentRunner::deploy_by_hash`](crate::runner::AgentRunner::deploy_by_hash) before [`RunnerBuilder::build`](RunnerBuilder::build).
pub struct Loading;

/// Terminal state: all dependencies wired, execution entrypoints available.
pub struct Ready;

/// Linear builder for the agent runner. Progression: Loading -> Ready.
pub struct RunnerBuilder<S> {
    pub(crate) runner: Arc<AgentRunner>,
    pub(crate) registry: Arc<RunnerRegistry>,
    conversation_history_tx: broadcast::Sender<ConversationHistoryUpdate>,
    _state: std::marker::PhantomData<S>,
}

impl RunnerBuilder<Loading> {
    /// Start building: parse config and create empty runner + registry.
    /// Registry is wired to the runner so discovery/A2A see agents as they are deployed.
    pub fn new(mut config: AgentRunnerConfig) -> Result<Self> {
        let (conversation_history_tx, _) = broadcast::channel(1024);
        config.conversation_history_notify = Some(conversation_history_tx.clone());
        let runner = Arc::new(AgentRunner::new(config)?);
        // Wire the internal A2A router to the runner for cross-agent dispatch.
        runner.internal_a2a_router().set_runner(Arc::clone(&runner));
        let registry = Arc::new(RunnerRegistry(Arc::clone(&runner)));
        Ok(Self {
            runner,
            registry,
            conversation_history_tx,
            _state: std::marker::PhantomData,
        })
    }

    /// Finish wiring and transition to Ready. Execution entrypoints unlock.
    pub fn build(self) -> RunnerBuilder<Ready> {
        RunnerBuilder {
            runner: self.runner,
            registry: self.registry,
            conversation_history_tx: self.conversation_history_tx,
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
    pub fn runner(&self) -> Arc<AgentRunner> {
        Arc::clone(&self.runner)
    }

    /// Broadcast sender wired to provenance commits and task lifecycle (conversation-history SSE).
    pub fn conversation_history_tx(&self) -> broadcast::Sender<ConversationHistoryUpdate> {
        self.conversation_history_tx.clone()
    }
}
