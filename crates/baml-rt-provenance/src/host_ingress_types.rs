// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed host ingress transcript identity parts (no stringly `unknown` buckets).

use baml_rt_core::{
    AgentDispatchRequest, AgentDispatchRoutingKey, EventSchemaVersion, ids::ContextId,
};
use serde::{Deserialize, Serialize};

/// Wire `a2a:host_ingress_kind` for operator transcript rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostIngressKind {
    SourcePollRecorded,
    DispatchAccepted,
    DispatchRejected,
    DispatchTransportError,
}

impl HostIngressKind {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::SourcePollRecorded => "source_poll_recorded",
            Self::DispatchAccepted => "dispatch_accepted",
            Self::DispatchRejected => "dispatch_rejected",
            Self::DispatchTransportError => "dispatch_transport_error",
        }
    }

    #[must_use]
    pub fn from_dispatch_failure(failure: HostDispatchFailureKind) -> Self {
        match failure {
            HostDispatchFailureKind::Rejected => Self::DispatchRejected,
            HostDispatchFailureKind::TransportError => Self::DispatchTransportError,
        }
    }
}

/// Agent rejection vs transport failure at the host dispatch boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostDispatchFailureKind {
    Rejected,
    TransportError,
}

impl HostDispatchFailureKind {
    #[must_use]
    pub const fn from_transport_flag(transport_failure: bool) -> Self {
        if transport_failure {
            Self::TransportError
        } else {
            Self::Rejected
        }
    }
}

/// Explicit source identity for dispatch outcome keys (never silent `"unknown"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HostIngressSourceRef {
    SourceRecords { kind: String, key: String },
    Unspecified,
}

impl HostIngressSourceRef {
    pub const UNSPECIFIED_KIND: &'static str = "_ingress_source_kind_unspecified";
    pub const UNSPECIFIED_KEY: &'static str = "_ingress_source_key_unspecified";

    #[must_use]
    pub fn from_dispatch_request(request: &AgentDispatchRequest) -> Self {
        match (&request.source_kind, &request.source_key) {
            (Some(kind), Some(key)) => Self::SourceRecords {
                kind: kind.as_str().to_string(),
                key: key.as_str().to_string(),
            },
            _ => Self::Unspecified,
        }
    }

    #[must_use]
    pub fn from_fields(source_kind: &str, source_key: &str) -> Self {
        Self::SourceRecords {
            kind: source_kind.to_string(),
            key: source_key.to_string(),
        }
    }

    #[must_use]
    pub fn kind_wire(&self) -> &str {
        match self {
            Self::SourceRecords { kind, .. } => kind.as_str(),
            Self::Unspecified => Self::UNSPECIFIED_KIND,
        }
    }

    #[must_use]
    pub fn key_wire(&self) -> &str {
        match self {
            Self::SourceRecords { key, .. } => key.as_str(),
            Self::Unspecified => Self::UNSPECIFIED_KEY,
        }
    }
}

/// Canonical parts for one host ingress poll transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIngressPollKey {
    pub context_id: ContextId,
    pub source_kind: String,
    pub source_key: String,
    pub source_cursor: String,
}

/// Canonical parts for one host dispatch outcome transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIngressDispatchOutcomeKey {
    pub context_id: ContextId,
    pub kind: HostIngressKind,
    pub routing_key: AgentDispatchRoutingKey,
    pub target_package: String,
    pub target_instance: String,
    pub source: HostIngressSourceRef,
}

/// Inputs for `ProvEvent::host_dispatch_rejected` (clippy-friendly).
#[derive(Debug, Clone)]
pub struct HostDispatchRejectedSpec {
    pub context_id: ContextId,
    pub routing_key: String,
    pub schema_version: EventSchemaVersion,
    pub target: baml_rt_core::DispatchTarget,
    pub source: HostIngressSourceRef,
    pub producer_key: Option<String>,
    pub detail: String,
    pub failure_kind: HostDispatchFailureKind,
}
