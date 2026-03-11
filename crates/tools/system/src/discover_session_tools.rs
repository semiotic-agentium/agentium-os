//! Handlers for browsing available agents and tools.

use std::{collections::BTreeSet, sync::Arc};

use baml_rt_core::AgentLister;
use baml_rt_tools::{
    ToolRegistry, create_multi_send_session_tool_from_async, tools::ToolFunctionMetadata,
};

use crate::tools::{
    AgentCardDto, DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput,
    ToolDiscoveryRecordDto,
};

fn card_to_dto(c: &baml_rt_core::AgentCard) -> AgentCardDto {
    AgentCardDto {
        name: c.name.clone(),
        version: c.version.clone(),
        agent_package: c.agent_package.clone(),
        agent_instance_id: c.agent_instance_id.clone(),
        tools: c.tools.clone(),
        description: c.description.clone(),
        capabilities: c.capabilities.clone(),
    }
}

fn filter_and_page(
    entries: &[baml_rt_core::AgentDiscoveryEntry],
    query: Option<&str>,
    required_capabilities: &BTreeSet<String>,
    limit: u32,
    offset: u32,
) -> Vec<AgentCardDto> {
    let query_lower = query.map(|q| q.to_lowercase());
    entries
        .iter()
        .map(|e| card_to_dto(&e.agent_card))
        .filter(|dto| {
            required_capabilities.iter().all(|required| {
                dto.capabilities
                    .iter()
                    .any(|capability| capability == required)
            })
        })
        .filter(|dto| {
            query_lower.as_ref().is_none_or(|q| {
                dto.name.to_lowercase().contains(q)
                    || dto.agent_package.to_lowercase().contains(q)
                    || dto
                        .description
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(q))
            })
        })
        .skip(offset as usize)
        .take(limit as usize)
        .collect()
}

fn normalize_required_capabilities(raw: Option<Vec<String>>) -> BTreeSet<String> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|capability| capability.trim().to_string())
        .filter(|capability| !capability.is_empty())
        .collect()
}

/// Creates the handler behind `system/discover_agents`.
pub fn discover_agents_handler(
    metadata: ToolFunctionMetadata,
    agent_list: Arc<dyn AgentLister>,
) -> std::sync::Arc<dyn baml_rt_tools::ToolHandler> {
    create_multi_send_session_tool_from_async::<
        DiscoverAgentsOpenInput,
        DiscoverAgentsSendInput,
        DiscoverAgentsNextOutput,
        _,
    >(metadata, move |send_input: DiscoverAgentsSendInput| {
        let list = agent_list.clone();
        Box::pin(async move {
            let limit = send_input.limit.unwrap_or(50).min(100);
            let offset = send_input.offset.unwrap_or(0);
            let required_capabilities =
                normalize_required_capabilities(send_input.required_capabilities);
            let entries = list.list_agents();
            let mut agents = filter_and_page(
                &entries,
                send_input.query.as_deref(),
                &required_capabilities,
                limit,
                offset,
            );
            // If query filtered out everything but we have agents, return first page of all (e.g. "which agents are ready?").
            if agents.is_empty()
                && !entries.is_empty()
                && send_input.query.is_some()
                && required_capabilities.is_empty()
            {
                agents = filter_and_page(&entries, None, &required_capabilities, limit, offset);
            }
            Ok(DiscoverAgentsNextOutput { agents, done: true })
        })
    })
}

/// Creates the handler behind `system/discover_tools`.
pub fn discover_tools_handler(
    metadata: ToolFunctionMetadata,
    tool_registry: Arc<ToolRegistry>,
) -> std::sync::Arc<dyn baml_rt_tools::ToolHandler> {
    create_multi_send_session_tool_from_async::<
        DiscoverToolsOpenInput,
        DiscoverToolsSendInput,
        DiscoverToolsNextOutput,
        _,
    >(metadata, move |send_input: DiscoverToolsSendInput| {
        let registry = tool_registry.clone();
        Box::pin(async move {
            let limit = send_input.limit.unwrap_or(50).min(100) as usize;
            let query = send_input.query.as_deref().unwrap_or("");
            let records = registry.search_tools(query, limit);
            let tools: Vec<ToolDiscoveryRecordDto> = records
                .into_iter()
                .map(|r| ToolDiscoveryRecordDto {
                    name: r.name.to_string(),
                    bundle: r.bundle.as_str().to_string(),
                    description: r.description,
                    tags: r.tags,
                })
                .collect();
            Ok(DiscoverToolsNextOutput { tools, done: true })
        })
    })
}
