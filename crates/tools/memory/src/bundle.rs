//! Memory bundle type and ToolBundle implementation.

use std::sync::Arc;

use baml_rt_core::Result;
use baml_rt_tools::{ToolBundle, ToolBundleMetadata, bundles::BundleType};

use crate::{handlers, manager::MemoryManager, metadata};

/// Memory bundle type marker.
pub struct Memory;

impl BundleType for Memory {
    const NAME: &'static str = "memory";

    fn description() -> &'static str {
        "Persistent graph-based cognitive memory for agents."
    }
}

/// Unified memory bundle: add, search, traverse, resolve, impact, link, stats.
pub struct MemoryBundle {
    manager: Arc<MemoryManager>,
}

impl MemoryBundle {
    /// Create a new memory bundle for the given agent.
    pub fn new(agent_name: &str) -> Result<Self> {
        let manager = MemoryManager::open_shared(agent_name).map_err(|e| {
            baml_rt_core::BamlRtError::InvalidArgument(format!(
                "failed to open memory for agent '{agent_name}': {e}"
            ))
        })?;
        Ok(Self { manager })
    }
}

impl ToolBundle for MemoryBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = baml_rt_tools::BundleName::new("memory".to_string())
            .expect("memory bundle name is valid");
        ToolBundleMetadata {
            name,
            description: "Persistent graph-based cognitive memory (add, search, traverse, resolve, impact, link, stats).".to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn baml_rt_tools::ToolHandler>> {
        vec![
            handlers::memory_add_handler(metadata::memory_add_metadata(), self.manager.clone()),
            handlers::memory_search_handler(
                metadata::memory_search_metadata(),
                self.manager.clone(),
            ),
            handlers::memory_traverse_handler(
                metadata::memory_traverse_metadata(),
                self.manager.clone(),
            ),
            handlers::memory_resolve_handler(
                metadata::memory_resolve_metadata(),
                self.manager.clone(),
            ),
            handlers::memory_impact_handler(
                metadata::memory_impact_metadata(),
                self.manager.clone(),
            ),
            handlers::memory_link_handler(metadata::memory_link_metadata(), self.manager.clone()),
            handlers::memory_stats_handler(metadata::memory_stats_metadata(), self.manager.clone()),
        ]
    }
}
