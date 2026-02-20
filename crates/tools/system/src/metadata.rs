//! Tool metadata registration for the system bundle (single mechanism: register_tool!).

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolHandler, parse_tool_name_and_class, register_tool,
    tools::{ToolFunctionMetadata, ToolMetadataBuilder, TypeBasedMetadataBuilder},
};

use crate::tools::{
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput,
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
        "Opens a session to another agent by route key. Send structured parts or text. Call Next repeatedly until the task is complete (session returns Done) or the output indicates INPUT_REQUIRED; then Finish. Each next() returns batched chunks.".to_string(),
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
        "Lists running agents (cards). Open session, then send query/limit/offset; next() returns one page. Send can be called multiple times. \
         query is a filter: only agents whose name, package, or description match the string are returned. To list all agents (e.g. 'who is available?', 'which agents are ready?'), omit query or send null—do not use a user phrase as the filter.",
    )
}

pub fn system_discover_tools_metadata() -> ToolFunctionMetadata {
    build_discover_metadata::<DiscoverToolsOpenInput, DiscoverToolsSendInput, DiscoverToolsNextOutput>(
        "system/discover_tools",
        "Searches registered tools by lexical rank. Open session, then send query/limit; next() returns results. Send can be called multiple times.",
    )
}

register_tool!(system_discover_agents_metadata, system_tool_build_unused);
register_tool!(system_discover_tools_metadata, system_tool_build_unused);
