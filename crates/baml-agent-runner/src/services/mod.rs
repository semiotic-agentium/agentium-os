// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! HTTP API service implementations bridging the provenance store to `baml-rt-api` traits.

pub(crate) mod context_index;
pub(crate) mod conversation_history;
pub(crate) mod episode;
pub(crate) mod host_ingress;
pub(crate) mod mermaid;
pub(crate) mod metrics;
pub(crate) mod observation;
pub(crate) mod observation_events;
pub(crate) mod planning;
pub(crate) mod provenance_ops;

pub(crate) use context_index::ContextIndexServiceImpl;
pub(crate) use conversation_history::ConversationHistoryServiceImpl;
pub(crate) use episode::EpisodeServiceImpl;
pub(crate) use host_ingress::HostIngressRecorderImpl;
pub(crate) use mermaid::MermaidServiceImpl;
pub(crate) use metrics::ContextMetricsServiceImpl;
pub(crate) use observation::ObservationServiceImpl;
pub(crate) use observation_events::ObservationEventServiceImpl;
pub(crate) use planning::PlanningServiceImpl;
pub(crate) use provenance_ops::ProvenanceOpsServiceImpl;
