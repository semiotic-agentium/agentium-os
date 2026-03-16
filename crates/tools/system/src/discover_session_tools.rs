//! discover_agents and discover_tools built via create_multi_send_session_tool_from_async
//! (open + multiple send/read).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use baml_rt_core::{
    AgentLister, EventSubscription, EventSubscriptionFilter, subscriptions_match_filter,
};
use baml_rt_tools::{
    ToolRegistry, create_multi_send_session_tool_from_async,
    tools::{HistoryContextV1, ToolFunctionMetadata},
};

use crate::tools::{
    AgentCardDto, AgentEventSubscriptionDto, DiscoverAgentsNextOutput, DiscoverAgentsOpenInput,
    DiscoverAgentsSendInput, DiscoverToolsNextOutput, DiscoverToolsOpenInput,
    DiscoverToolsSendInput, ToolDiscoveryRecordDto,
};

fn subscription_to_dto(subscription: &EventSubscription) -> AgentEventSubscriptionDto {
    AgentEventSubscriptionDto {
        schema_versions: subscription
            .schema_versions
            .iter()
            .map(ToString::to_string)
            .collect(),
        source_kinds: subscription
            .source_kinds
            .iter()
            .map(ToString::to_string)
            .collect(),
        source_keys: subscription
            .source_keys
            .iter()
            .map(ToString::to_string)
            .collect(),
        source_key_prefixes: subscription
            .source_key_prefixes
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

fn card_to_dto(c: &baml_rt_core::AgentCard) -> AgentCardDto {
    AgentCardDto {
        name: c.name.clone(),
        version: c.version.clone(),
        agent_package: c.agent_package.clone(),
        agent_instance_id: c.agent_instance_id.clone(),
        tools: c.tools.clone(),
        description: c.description.clone(),
        capabilities: c.capabilities.clone(),
        subscriptions: c.subscriptions.iter().map(subscription_to_dto).collect(),
    }
}

fn filter_and_page(
    entries: &[baml_rt_core::AgentDiscoveryEntry],
    query: Option<&str>,
    required_capabilities: &BTreeSet<String>,
    subscription_filter: &EventSubscriptionFilter,
    limit: u32,
    offset: u32,
) -> Vec<AgentCardDto> {
    let query_lower = query.map(|q| q.to_lowercase());
    entries
        .iter()
        .filter(|entry| {
            required_capabilities.iter().all(|required| {
                entry
                    .agent_card
                    .capabilities
                    .iter()
                    .any(|capability| capability.trim().to_lowercase() == *required)
            })
        })
        .filter(|entry| {
            subscriptions_match_filter(&entry.agent_card.subscriptions, subscription_filter)
        })
        .filter(|entry| {
            query_lower.as_ref().is_none_or(|q| {
                entry.agent_card.name.to_lowercase().contains(q)
                    || entry.agent_card.agent_package.to_lowercase().contains(q)
                    || entry
                        .agent_card
                        .description
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(q))
            })
        })
        .map(|entry| card_to_dto(&entry.agent_card))
        .skip(offset as usize)
        .take(limit as usize)
        .collect()
}

fn normalize_required_capabilities(raw: Option<Vec<String>>) -> BTreeSet<String> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|capability| capability.trim().to_lowercase())
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
            let normalized_query = send_input
                .query
                .as_deref()
                .map(str::trim)
                // Treat empty/sentinel values as "no filter" to default to broad discovery.
                .filter(|q| !q.is_empty() && !q.eq_ignore_ascii_case("null") && *q != "*");
            let required_capabilities =
                normalize_required_capabilities(send_input.required_capabilities);
            let subscription_filter = EventSubscriptionFilter::new(
                send_input.required_schema_versions.unwrap_or_default(),
                send_input.required_source_kinds.unwrap_or_default(),
            );
            let entries = list.list_agents();
            let mut agents = filter_and_page(
                &entries,
                normalized_query,
                &required_capabilities,
                &subscription_filter,
                limit,
                offset,
            );
            // If query filtered out everything but we have agents, return first page of all (e.g. "which agents are ready?").
            if agents.is_empty()
                && !entries.is_empty()
                && send_input.query.is_some()
                && required_capabilities.is_empty()
                && subscription_filter.is_empty()
            {
                agents = filter_and_page(
                    &entries,
                    None,
                    &required_capabilities,
                    &subscription_filter,
                    limit,
                    offset,
                );
            }
            Ok(DiscoverAgentsNextOutput {
                agents: agents.clone(),
                done: true,
                history_context: Some(HistoryContextV1 {
                    hop: 1,
                    op: "Read".to_string(),
                    status: "done".to_string(),
                    truncated: false,
                    cursor: None,
                    payload: Some(BTreeMap::from([
                        ("count".to_string(), serde_json::json!(agents.len())),
                        ("query".to_string(), serde_json::json!(normalized_query)),
                    ])),
                }),
            })
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
            Ok(DiscoverToolsNextOutput {
                tools: tools.clone(),
                done: true,
                history_context: Some(HistoryContextV1 {
                    hop: 1,
                    op: "Read".to_string(),
                    status: "done".to_string(),
                    truncated: false,
                    cursor: None,
                    payload: Some(BTreeMap::from([
                        ("count".to_string(), serde_json::json!(tools.len())),
                        ("query".to_string(), serde_json::json!(send_input.query)),
                    ])),
                }),
            })
        })
    })
}
