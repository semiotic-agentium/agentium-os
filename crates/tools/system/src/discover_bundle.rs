// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Bundle exposing system/discover_agents and system/discover_tools.

use std::sync::Arc;

use baml_rt_core::AgentLister;
use baml_rt_tools::{ToolBundle, ToolBundleMetadata, ToolRegistry};

use crate::{
    discover_session_tools::{discover_agents_handler, discover_tools_handler},
    metadata::{system_discover_agents_metadata, system_discover_tools_metadata},
};

/// Bundle exposing discover_agents and discover_tools (requires agent list + tool registry).
pub struct DiscoverBundle {
    agent_list: Arc<dyn AgentLister>,
    tool_registry: Arc<ToolRegistry>,
}

impl DiscoverBundle {
    pub fn new(agent_list: Arc<dyn AgentLister>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            agent_list,
            tool_registry,
        }
    }
}

impl ToolBundle for DiscoverBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = baml_rt_tools::BundleName::new("system".to_string())
            .expect("system bundle name is valid");
        ToolBundleMetadata {
            name,
            description: "System discovery tools (agents, tools).".to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn baml_rt_tools::ToolHandler>> {
        vec![
            discover_agents_handler(system_discover_agents_metadata(), self.agent_list.clone()),
            discover_tools_handler(system_discover_tools_metadata(), self.tool_registry.clone()),
        ]
    }
}
