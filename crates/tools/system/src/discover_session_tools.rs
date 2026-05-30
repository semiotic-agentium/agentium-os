// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! discover_agents and discover_tools built via create_multi_send_session_tool_from_async
//! (open + multiple send/read).

use std::{collections::BTreeSet, sync::Arc};

use baml_rt_core::{
    AgentLister, EventSubscription, EventSubscriptionFilter, subscriptions_match_filter,
};
use baml_rt_tools::{
    ToolRegistry, create_multi_send_session_tool_from_async, opaque_json_map_from_object,
    tools::{
        HistoryContextSessionOp, HistoryContextStatus, HistoryContextV1, ToolFunctionMetadata,
    },
};
use tracing::info;

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
        content_hash: c.content_hash.clone(),
        repository_version: c.repository_version,
        agent_package: c.agent_package.clone(),
        agent_instance_id: c.agent_instance_id.clone(),
        tools: c.tools.clone(),
        description: c.description.clone(),
        capabilities: c.capabilities.clone(),
        tags: c.tags.clone(),
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
            let registry_count = entries.len();
            let mut agents = filter_and_page(
                &entries,
                normalized_query,
                &required_capabilities,
                &subscription_filter,
                limit,
                offset,
            );
            let pre_fallback_count = agents.len();
            // If a meaningful query produced no rows (not merely empty pagination), but the registry
            // is non-empty and no strict filters are set, return the same page of all agents.
            // Use `normalized_query` (not raw `query`) so sentinels/whitespace do not skew this.
            // Only when `offset == 0`: a non-zero offset can be "past end" of matches and must not
            // trigger fallback.
            let mut fallback_applied = false;
            if agents.is_empty()
                && registry_count > 0
                && offset == 0
                && normalized_query.is_some()
                && required_capabilities.is_empty()
                && subscription_filter.is_empty()
            {
                fallback_applied = true;
                agents = filter_and_page(
                    &entries,
                    None,
                    &required_capabilities,
                    &subscription_filter,
                    limit,
                    offset,
                );
            }
            let post_fallback_count = agents.len();
            info!(
                registry_count = registry_count,
                query = normalized_query.unwrap_or(""),
                pre_fallback_count = pre_fallback_count,
                post_fallback_count = post_fallback_count,
                fallback_applied = fallback_applied,
                "discover_agents completed"
            );
            Ok(DiscoverAgentsNextOutput {
                agents: agents.clone(),
                done: true,
                history_context: Some(HistoryContextV1 {
                    hop: 1,
                    op: HistoryContextSessionOp::PageRead,
                    status: HistoryContextStatus::Done,
                    truncated: false,
                    cursor: None,
                    payload: Some(opaque_json_map_from_object(serde_json::json!({
                        "count": post_fallback_count,
                        "returned_count": post_fallback_count,
                        "registry_count": registry_count,
                        "fallback_applied": fallback_applied,
                        "query": normalized_query
                    }))),
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
                    event_sources: r.event_sources.iter().map(|s| s.to_string()).collect(),
                })
                .collect();
            Ok(DiscoverToolsNextOutput {
                tools: tools.clone(),
                done: true,
                history_context: Some(HistoryContextV1 {
                    hop: 1,
                    op: HistoryContextSessionOp::PageRead,
                    status: HistoryContextStatus::Done,
                    truncated: false,
                    cursor: None,
                    payload: Some(opaque_json_map_from_object(serde_json::json!({
                        "count": tools.len(),
                        "query": send_input.query
                    }))),
                }),
            })
        })
    })
}
