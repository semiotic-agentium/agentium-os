// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! System tool bundle for the BAML runtime.
//!
//! Tools live in **crates/tools** and are **not** built into the agent. The host (runner)
//! composes the tool catalogue at startup; agents are started then. The relationship between
//! an agent and its tools is **indirect — mediated by the host**. This crate provides the
//! unified system bundle (internal_a2a, discover_agents, discover_tools); the host registers
//! [`SystemBundle`] when booting each agent.

mod a2a_session;
pub mod bundle;
mod callback_bundle;
pub mod callback_delivery_gate;
pub mod callback_dispatch_message;
pub mod callback_producer;
pub mod callback_store;
mod discover_bundle;
mod discover_session_tools;
mod event_source_type;
pub mod metadata;
mod provenance_bundle;
mod provenance_session_tools;
pub mod tools;

pub use a2a_session::A2aSessionBundle;
pub use bundle::{System, SystemBundle};
pub use callback_bundle::CallbackBundle;
pub use provenance_bundle::ProvenanceBundle;
pub use tools::{
    AgentCardDto, AgentEventSubscriptionDto, CallbackCancelInput, CallbackCancelledOutput,
    CallbackContinuationMode, CallbackScheduleInput, CallbackScheduledOutput, CallbackToolInput,
    CallbackToolOutput, ConversationChunk, ConversationMessage, ConversationPart,
    DiscoverAgentsNextOutput, DiscoverAgentsOpenInput, DiscoverAgentsSendInput,
    DiscoverToolsNextOutput, DiscoverToolsOpenInput, DiscoverToolsSendInput, InternalA2aCompletion,
    InternalA2aNextOutput, InternalA2aOpenInput, InternalA2aSendInput, InternalA2aTarget,
    ProvenanceQueryNextOutput, ProvenanceQueryOpenInput, ProvenanceQuerySendInput,
    ToolDiscoveryRecordDto,
};
