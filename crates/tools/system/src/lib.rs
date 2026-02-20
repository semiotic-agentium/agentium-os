//! System tool bundle for the BAML runtime.
//!
//! Tools live in **crates/tools** and are **not** built into the agent. The host (runner)
//! composes the tool catalogue at startup; agents are started then. The relationship between
//! an agent and its tools is **indirect — mediated by the host**. This crate provides the
//! unified system bundle (internal_a2a, discover_agents, discover_tools); the host registers
//! [`SystemBundle`] when booting each agent.

mod a2a_session;
pub mod bundle;
mod discover_bundle;
mod discover_session_tools;
pub mod metadata;
pub mod tools;

pub use a2a_session::A2aSessionBundle;
pub use bundle::{System, SystemBundle};
pub use tools::{
    AgentCardDto, ConversationChunk, ConversationMessage, ConversationPart,
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverToolsNextOutput,
    DiscoverToolsOpenInput, InternalA2aCompletion, InternalA2aNextOutput, InternalA2aOpenInput,
    InternalA2aSendInput, InternalA2aTarget, ToolDiscoveryRecordDto,
};
