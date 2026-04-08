//! CLI command implementations.

pub(crate) mod agent_discovery;
pub mod build;
pub mod chat;
pub mod deploy;
pub mod doctor;
pub mod list_agents;
pub mod list_deployed_instances;
pub mod list_event_sources;
pub mod list_tools;
pub mod new_agent;
pub mod new_tool;
pub mod publish;
pub mod push;
pub mod regen;
pub mod undeploy;
pub(crate) mod utils;
