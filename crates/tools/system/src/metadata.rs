//! User-facing metadata for the system tool bundle.

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    SessionPolicy, ToolHandler, parse_tool_name_and_class, register_tool,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

use crate::tools::{
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput, ProvenanceQueryNextOutput,
    ProvenanceQueryOpenInput, ProvenanceQuerySendInput,
};

fn system_tool_build_unused() -> Result<Arc<dyn ToolHandler>> {
    Err(BamlRtError::InvalidArgument(
        "System tools are registered by the host via SystemBundle".to_string(),
    ))
}

fn build_a2a_metadata(tool_name: &str) -> ToolFunctionMetadata {
    let (name, class_name) =
        parse_tool_name_and_class(tool_name).expect("a2a tool name is a compile-time constant");
    TypeBasedMetadataBuilder::<InternalA2aOpenInput, InternalA2aSendInput, InternalA2aNextOutput>::new(
        name,
        class_name,
        "Opens a conversational session to another agent. Send a message (text or parts), Read the response, then Send follow-ups or Finish. Multiple Send/Read rounds are allowed within one session — use this for multi-turn conversations with the delegated agent.".to_string(),
    )
    .with_tags(vec!["system".to_string(), "a2a".to_string()])
    .with_session_policy(SessionPolicy::MultiSend)
    .with_projection_semantics(
        "Chunk envelope and task-state identity only, without full message text bodies.",
        "Compact stream digest: chunk counts, completion state, and high-level task movement.",
        "Full batched conversation chunks and status/artifact updates for this read hop.",
    )
    .build_metadata()
}

pub fn system_internal_a2a_metadata() -> ToolFunctionMetadata {
    build_a2a_metadata("system/internal_a2a")
}

register_tool!(system_internal_a2a_metadata, system_tool_build_unused);

fn build_discover_metadata<Open, SendInput, Next>(
    tool_name: &str,
    description: &str,
) -> ToolFunctionMetadata
where
    Open: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
    SendInput: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
    Next: schemars::JsonSchema + ts_rs::TS + Send + Sync + 'static,
{
    let (name, class_name) = parse_tool_name_and_class(tool_name)
        .expect("discover tool name is a compile-time constant");
    TypeBasedMetadataBuilder::<Open, SendInput, Next>::new(
        name,
        class_name,
        description.to_string(),
    )
    .with_tags(vec!["system".to_string(), "discovery".to_string()])
    .with_projection_semantics(
        "Identifiers only for discovered entities (agent or tool names and stable ids).",
        "Compact list summary for this read hop (count and query constraints).",
        "Full read payload for the hop (paged agent cards or tool records).",
    )
    .build_metadata()
}

pub fn system_discover_agents_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<
        DiscoverAgentsOpenInput,
        DiscoverAgentsSendInput,
        DiscoverAgentsNextOutput,
    >(
        "system/discover_agents",
        "Browse available agents. You can optionally filter by query, requiredCapabilities, or matching event subscriptions. Omit filters to list all agents.",
    )
}

pub fn system_discover_tools_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<DiscoverToolsOpenInput, DiscoverToolsSendInput, DiscoverToolsNextOutput>(
        "system/discover_tools",
        "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times.",
    )
}

pub fn system_introspection_metadata() -> ToolFunctionMetadata {
    let (name, class_name) =
        parse_tool_name_and_class("system/introspection").expect("static tool name");
    TypeBasedMetadataBuilder::<
        ProvenanceQueryOpenInput,
        ProvenanceQuerySendInput,
        ProvenanceQueryNextOutput,
    >::new(
        name,
        class_name,
        "Queries provenance rows for the current context with compact token-aware output. Open session, send filters/grouping, then read() returns one result page.".to_string(),
    )
    .with_tags(vec!["system".to_string(), "discovery".to_string()])
    .with_projection_semantics(
        "Only addressing graph: current traversal ref plus reachable refs, without payload bodies.",
        "Compact aggregate over the selected ref: counts/totals and source kinds, without full payload bodies.",
        "Full selected archive payload for the requested ref, including typed payload records.",
    )
    .build_metadata()
}

pub fn system_extrospection_metadata() -> ToolFunctionMetadata {
    let (name, class_name) =
        parse_tool_name_and_class("system/extrospection").expect("static tool name");
    TypeBasedMetadataBuilder::<
        ProvenanceQueryOpenInput,
        ProvenanceQuerySendInput,
        ProvenanceQueryNextOutput,
    >::new(
        name,
        class_name,
        "Queries provenance rows across contexts and agents with compact token-aware output. Open session, send filters/grouping, then read() returns one result page.".to_string(),
    )
    .with_tags(vec!["system".to_string(), "discovery".to_string()])
    .with_projection_semantics(
        "Only addressing graph: current traversal ref plus reachable refs, without payload bodies.",
        "Compact aggregate over the selected ref: counts/totals and source kinds, without full payload bodies.",
        "Full selected archive payload for the requested ref, including typed payload records.",
    )
    .build_metadata()
}

register_tool!(system_discover_agents_metadata, system_tool_build_unused);
register_tool!(system_discover_tools_metadata, system_tool_build_unused);
register_tool!(system_introspection_metadata, system_tool_build_unused);
register_tool!(system_extrospection_metadata, system_tool_build_unused);
