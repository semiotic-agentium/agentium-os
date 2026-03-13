//! User-facing metadata for the system tool bundle.

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    BundleName, ToolConfigMetadata, ToolHandler, parse_tool_name_and_class, register_tool,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};
use serde_json::to_value;

use crate::tools::{
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput, ProvenanceQueryNextOutput,
    ProvenanceQueryOpenInput, ProvenanceQuerySendInput, WorkflowRoutingConfig,
    WorkflowRoutingNextOutput, WorkflowRoutingOpenInput, WorkflowRoutingSendInput,
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
        "Starts a conversation with another agent by route key. Send text or structured parts, then call Next until the agent is done or asks for more input.".to_string(),
    )
    .with_tags(vec!["system".to_string(), "a2a".to_string()])
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
    .build_metadata()
}

pub fn system_discover_agents_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<
        DiscoverAgentsOpenInput,
        DiscoverAgentsSendInput,
        DiscoverAgentsNextOutput,
    >(
        "system/discover_agents",
        "Browse available agents. You can optionally filter by query or requiredCapabilities. Omit query to list all agents.",
    )
}

pub fn system_discover_tools_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<DiscoverToolsOpenInput, DiscoverToolsSendInput, DiscoverToolsNextOutput>(
        "system/discover_tools",
        "Browse available tools. You can optionally filter by query and limit the number of results.",
    )
}

pub fn system_workflow_routing_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("system/workflow_routing")
        .expect("workflow routing tool name is a compile-time constant");
    let config_schema = schemars::schema_for!(WorkflowRoutingConfig);
    let config_default = to_value(WorkflowRoutingConfig::default())
        .expect("workflow routing config default must serialize");
    TypeBasedMetadataBuilder::<
        WorkflowRoutingOpenInput,
        WorkflowRoutingSendInput,
        WorkflowRoutingNextOutput,
    >::new(
        name,
        class_name,
        "Looks up downstream routing policy for a workflow event based on typed config. Send the event decision and source details, then read back the required capabilities and optional preferred agent package.".to_string(),
    )
    .with_tags(vec![
        "system".to_string(),
        "workflow".to_string(),
        "routing".to_string(),
    ])
    .with_config(ToolConfigMetadata::new(
        to_value(config_schema).expect("workflow routing config schema must serialize"),
        config_default,
        Some("WorkflowRoutingConfig".to_string()),
    ))
    .with_config_bundle(BundleName::new("workflow_routing").expect("workflow routing bundle name"))
    .build_metadata()
}

pub fn system_introspection_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<
        ProvenanceQueryOpenInput,
        ProvenanceQuerySendInput,
        ProvenanceQueryNextOutput,
    >(
        "system/introspection",
        "Queries provenance rows for the current context with compact token-aware output. Open session, send filters/grouping, then next() returns one result page.",
    )
}

pub fn system_extrospection_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<
        ProvenanceQueryOpenInput,
        ProvenanceQuerySendInput,
        ProvenanceQueryNextOutput,
    >(
        "system/extrospection",
        "Queries provenance rows across contexts and agents with compact token-aware output. Open session, send filters/grouping, then next() returns one result page.",
    )
}

register_tool!(system_discover_agents_metadata, system_tool_build_unused);
register_tool!(system_discover_tools_metadata, system_tool_build_unused);
register_tool!(system_workflow_routing_metadata, system_tool_build_unused);
register_tool!(system_introspection_metadata, system_tool_build_unused);
register_tool!(system_extrospection_metadata, system_tool_build_unused);
