// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Core vocabulary types for the event-producer / host-dispatch pipeline.
//!
//! A [`ProducedEvent`] is the normalized output of an event producer. It converts
//! losslessly to [`PublishedEvent`](crate::event_subscription::PublishedEvent) for
//! subscription matching and to [`AgentDispatchRequest`] for delivery.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentDispatchRequest, AgentDispatchRoutingKey, AgentRouteKey, BamlRtError, EventSchemaVersion,
    Result,
    event_subscription::{EventSourceKey, EventSourceKind, PublishedEvent},
    host_source_records_schema_version,
    host_wire::wire,
    ids::{ContextId, TaskId},
};

/// One event emitted by a producer, ready for the host to match and deliver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducedEvent {
    /// Routing key for dispatch (e.g. `slack:intake`).
    pub routing_key: AgentDispatchRoutingKey,
    /// Schema version / message family (e.g. `host.source-records.v1`).
    pub schema_version: EventSchemaVersion,
    /// Source kind for subscription matching (e.g. `slack`).
    pub source_kind: EventSourceKind,
    /// Exact source key for narrow matching (e.g. `slack:C123`).
    pub source_key: EventSourceKey,
    /// Opaque event payloads delivered to subscribers.
    pub messages: Vec<Value>,
    /// Optional context for provenance continuity.
    pub context_id: Option<ContextId>,
    /// Optional task for provenance continuity.
    pub task_id: Option<TaskId>,
    /// Optional caller-supplied message id for provenance threading.
    pub message_id: Option<String>,
    /// Optional transport metadata.
    pub metadata: Option<Value>,
}

impl ProducedEvent {
    /// Build a `host.source-records.v1` event for pub/sub delivery.
    pub fn host_source_records(
        source_kind: EventSourceKind,
        source_key: EventSourceKey,
        payload: Value,
        message_id: Option<String>,
        metadata: Option<Value>,
    ) -> Result<Self> {
        let routing_key = AgentDispatchRoutingKey::parse(wire::SOURCE_RECORDS_ROUTING_KEY)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "invalid source-records routing key: {}",
                    wire::SOURCE_RECORDS_ROUTING_KEY
                ))
            })?;
        Ok(Self {
            routing_key,
            schema_version: host_source_records_schema_version(),
            source_kind,
            source_key,
            messages: vec![payload],
            context_id: None,
            task_id: None,
            message_id,
            metadata,
        })
    }

    /// Build the [`PublishedEvent`] descriptor used for subscription matching.
    pub fn as_published_event(&self) -> PublishedEvent {
        // All constituent types are pre-validated at construction, so we can
        // build directly without going through the fallible try_new path.
        PublishedEvent {
            schema_version: self.schema_version.clone(),
            source_kind: self.source_kind.clone(),
            source_key: self.source_key.clone(),
        }
    }

    /// Consume this event into an [`AgentDispatchRequest`] for delivery.
    pub fn into_dispatch_request(self) -> AgentDispatchRequest {
        AgentDispatchRequest {
            routing_key: self.routing_key,
            message_type: self.schema_version,
            messages: self.messages,
            context_id: self.context_id,
            task_id: self.task_id,
            message_id: self.message_id,
            metadata: self.metadata,
        }
    }
}

/// Per-subscriber delivery failure (rejection or transport error).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriberDeliveryFailure {
    /// Agent returned `accepted: false` with an optional detail string.
    Rejected { detail: String },
    /// Host dispatch to the agent route failed (`BamlRtError` captured at fan-out).
    Dispatch { detail: String },
}

impl SubscriberDeliveryFailure {
    /// Human-readable detail for logs and HTTP responses.
    pub fn detail(&self) -> String {
        match self {
            Self::Rejected { detail } | Self::Dispatch { detail } => detail.clone(),
        }
    }

    pub fn from_dispatch_error(err: BamlRtError) -> Self {
        Self::Dispatch {
            detail: err.to_string(),
        }
    }
}

/// Aggregate outcome of dispatching one [`ProducedEvent`] to all matched subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDeliveryOutcome {
    /// Number of agents whose subscriptions matched the event.
    pub subscribers_matched: usize,
    /// Number of agents that accepted the dispatch.
    pub subscribers_accepted: usize,
    /// Per-subscriber failures: route key and failure reason.
    pub failures: Vec<(AgentRouteKey, SubscriberDeliveryFailure)>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ProducedEvent;
    use crate::{
        AgentDispatchRoutingKey, EventSchemaVersion,
        event_subscription::{EventSourceKey, EventSourceKind},
    };

    fn test_event() -> ProducedEvent {
        ProducedEvent {
            routing_key: AgentDispatchRoutingKey::parse("slack:intake").unwrap(),
            schema_version: EventSchemaVersion::parse("test.v1").unwrap(),
            source_kind: EventSourceKind::parse("slack").unwrap(),
            source_key: EventSourceKey::parse("slack:#general").unwrap(),
            messages: vec![json!({"hello": "world"})],
            context_id: None,
            task_id: None,
            message_id: Some("msg-001".into()),
            metadata: None,
        }
    }

    #[test]
    fn as_published_event_preserves_matching_fields() {
        let event = test_event();
        let published = event.as_published_event();

        assert_eq!(published.schema_version.as_str(), "test.v1");
        assert_eq!(published.source_kind.as_str(), "slack");
        assert_eq!(published.source_key.as_str(), "slack:#general");
    }

    #[test]
    fn into_dispatch_request_maps_all_fields() {
        let event = test_event();
        let request = event.into_dispatch_request();

        assert_eq!(request.routing_key.as_str(), "slack:intake");
        assert_eq!(request.message_type.as_str(), "test.v1");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.message_id.as_deref(), Some("msg-001"));
        assert!(request.context_id.is_none());
        assert!(request.task_id.is_none());
        assert!(request.metadata.is_none());
    }
}
