//! System bundle type and unified system tool bundle.

use std::sync::Arc;

use baml_rt_core::{A2aRequestHandler, AgentLister};
use baml_rt_tools::{ToolBundle, ToolBundleMetadata, ToolRegistry, bundles::BundleType};

use crate::{a2a_session::A2aSessionBundle, discover_bundle::DiscoverBundle};

/// System bundle — host tools for system operations (name/description only).
pub struct System;

impl BundleType for System {
    const NAME: &'static str = "system";

    fn description() -> &'static str {
        "System tools (A2A conversation, agent discovery, tool discovery)."
    }
}

/// Unified system bundle: internal_a2a, discover_agents, discover_tools.
/// Register this with the host tool registry so agents get all three tools.
pub struct SystemBundle {
    a2a: A2aSessionBundle,
    discover: DiscoverBundle,
}

impl SystemBundle {
    pub fn new(
        agent_list: Arc<dyn AgentLister>,
        tool_registry: Arc<ToolRegistry>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
    ) -> Self {
        Self {
            a2a: A2aSessionBundle::new(a2a_handler),
            discover: DiscoverBundle::new(agent_list, tool_registry),
        }
    }
}

impl ToolBundle for SystemBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = baml_rt_tools::BundleName::new("system".to_string())
            .expect("system bundle name is valid");
        ToolBundleMetadata {
            name,
            description: "System tools (A2A session, agent discovery, tool discovery).".to_string(),
            config_schema: None,
            secret_requirements: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn baml_rt_tools::ToolHandler>> {
        let mut fns = self.a2a.functions();
        fns.extend(self.discover.functions());
        fns
    }
}
