// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! A2A protocol support.

#![recursion_limit = "256"]

pub mod a2a;
pub mod a2a_store;
pub mod a2a_transport;
pub mod a2a_types;
pub mod agent_registry;
pub mod auto_status;
pub mod dispatch_port;
pub mod error_classifier;
pub mod error_mapping;
pub mod event_dispatcher;
pub mod events;
pub mod handlers;
pub(crate) mod live_stream;
pub(crate) mod live_stream_working_relay;
pub(crate) mod provenance_notify_writer;
pub mod request_router;
pub mod response;
pub mod result_deduplicator;
pub mod result_extractor;
pub mod result_pipeline;
pub mod result_processor;
pub mod task_subgraph_store;
pub mod task_update_broadcaster;
pub mod task_update_drain;
pub mod task_update_session;
pub(crate) mod wire;

pub use a2a::{A2aMethod, A2aOutcome, A2aRequest};
pub use a2a_transport::{
    A2aAgent, A2aAgentBuilder, RegistrationMode, install_provenance_conversation_wiring,
};
pub use agent_registry::AgentRegistry;
pub use baml_rt_core::{A2aJsChatHost, A2aRequestHandler};
pub use dispatch_port::RegistryDispatchPort;
pub use event_dispatcher::EventDispatcher;
