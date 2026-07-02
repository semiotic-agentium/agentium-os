// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! CLI command implementations.

pub mod a2a_http;
pub(crate) mod agent_discovery;
pub mod chat;
pub mod check_external_tool;
pub mod config_cmd;
pub mod deploy;
pub mod doctor;
pub mod export_snapshot_cache;
pub mod external_tool;
pub mod init;
pub mod install;
pub mod list_agents;
pub mod list_deployed_instances;
pub mod list_event_sources;
pub mod list_tools;
pub mod mcp;
pub mod new_agent;
pub mod new_static_tool;
pub mod new_tool;
pub mod publish;
pub mod push;
pub mod sandbox_bind_sync;
pub mod sandbox_oci_prepare;
pub mod snapshot_report;
pub mod sync_types;
pub mod undeploy;
pub(crate) mod utils;
