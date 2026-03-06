//! Tool metadata registration for the system bundle.

use baml_rt_tools::baml_tool;

use crate::tools::{
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput, ProvenanceQueryNextOutput,
    ProvenanceQueryOpenInput, ProvenanceQuerySendInput,
};

#[baml_tool(
    name = "system/internal_a2a",
    description = "Opens a session to another agent by route key. Send structured parts or text. \
        Call Next repeatedly until the task is complete (session returns Done) or the output \
        indicates INPUT_REQUIRED; then Finish. Each next() returns batched chunks.",
    tags = ["system", "a2a"],
    metadata_only,
    open_input = InternalA2aOpenInput,
    input = InternalA2aSendInput,
    output = InternalA2aNextOutput,
)]
pub struct SystemInternalA2a;

#[baml_tool(
    name = "system/discover_agents",
    description = "Lists running agents (cards). Open session, then send query/limit/offset; \
        next() returns one page. Send can be called multiple times. query is a filter: only agents \
        whose name, package, or description match the string are returned. To list all agents \
        (e.g. 'who is available?', 'which agents are ready?'), omit query or send null—do not use \
        a user phrase as the filter.",
    tags = ["system", "discovery"],
    metadata_only,
    open_input = DiscoverAgentsOpenInput,
    input = DiscoverAgentsSendInput,
    output = DiscoverAgentsNextOutput,
)]
pub struct SystemDiscoverAgents;

<<<<<<< HEAD
#[baml_tool(
    name = "system/discover_tools",
    description = "Searches registered tools by lexical rank. Open session, then send query/limit; \
        next() returns results. Send can be called multiple times.",
    tags = ["system", "discovery"],
    metadata_only,
    open_input = DiscoverToolsOpenInput,
    input = DiscoverToolsSendInput,
    output = DiscoverToolsNextOutput,
)]
pub struct SystemDiscoverTools;
=======
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
register_tool!(system_introspection_metadata, system_tool_build_unused);
register_tool!(system_extrospection_metadata, system_tool_build_unused);
>>>>>>> 7fbad689 (feat(provenance): add ops query API and session tools with typed-id boundaries)
