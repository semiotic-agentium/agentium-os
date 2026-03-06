//! Tool metadata registration for the system bundle.

use baml_rt_tools::baml_tool;

use crate::tools::{
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aNextOutput,
    InternalA2aOpenInput, InternalA2aSendInput,
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
