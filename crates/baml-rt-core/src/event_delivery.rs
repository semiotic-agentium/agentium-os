//! Shared host pub/sub: subscription matching and fan-out dispatch to agents.
//!
//! [`matching_subscriber_routes`] and [`deliver_to_subscribers`] are the single
//! delivery primitive used by in-process event dispatch and HTTP publish ingress.

use async_trait::async_trait;
use tracing::warn;

use crate::{
    AgentDiscoveryEntry, AgentDispatchAck, AgentDispatchRequest, AgentInstanceId, AgentPackageName,
    AgentRouteKey, EventDeliveryOutcome, ProducedEvent, Result, SubscriberAcceptance,
    SubscriberDeliveryFailure,
    event_subscription::{PublishedEvent, subscriptions_match_published_event},
};

/// Index of deployable agents for subscription matching.
pub struct SubscriberIndex<'a> {
    entries: &'a [AgentDiscoveryEntry],
}

impl<'a> SubscriberIndex<'a> {
    pub fn new(entries: &'a [AgentDiscoveryEntry]) -> Self {
        Self { entries }
    }

    /// Route keys for agents whose subscriptions match the published event descriptor.
    pub fn matching_routes(&self, event: &PublishedEvent) -> Vec<AgentRouteKey> {
        matching_subscriber_routes(self.entries, event)
    }
}

/// Find agents whose subscriptions match a published event (subscription fields only).
pub fn matching_subscriber_routes(
    entries: &[AgentDiscoveryEntry],
    event: &PublishedEvent,
) -> Vec<AgentRouteKey> {
    entries
        .iter()
        .filter(|entry| subscriptions_match_published_event(&entry.agent_card.subscriptions, event))
        .filter_map(|entry| {
            let package = AgentPackageName::parse(&entry.agent_package);
            let instance = AgentInstanceId::parse(&entry.agent_instance_id);
            match (package, instance) {
                (Some(p), Some(i)) => Some(AgentRouteKey::new(p, i)),
                _ => {
                    warn!(
                        agent_package = %entry.agent_package,
                        agent_instance_id = %entry.agent_instance_id,
                        "matched subscriber has invalid route key and will be skipped"
                    );
                    None
                }
            }
        })
        .collect()
}

/// Port for delivering a dispatch request to one agent route.
#[async_trait]
pub trait AgentDispatchPort: Send + Sync {
    async fn dispatch(
        &self,
        target: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck>;
}

/// Publish through the host pub/sub spine: build a subscriber index from discovery
/// entries, match subscriptions, and dispatch to agents.
pub async fn publish_to_subscribers(
    entries: &[AgentDiscoveryEntry],
    event: ProducedEvent,
    port: &dyn AgentDispatchPort,
) -> Result<EventDeliveryOutcome> {
    DiscoveryPublishClient { entries, port }
        .publish(event)
        .await
}

/// Shared publish semantics for HTTP `/events/publish` and in-process dispatch.
#[async_trait]
pub trait HostPublishClient: Send + Sync {
    async fn publish(&self, event: ProducedEvent) -> Result<EventDeliveryOutcome>;
}

/// [`HostPublishClient`] backed by a discovery snapshot and [`AgentDispatchPort`].
pub struct DiscoveryPublishClient<'a> {
    pub entries: &'a [AgentDiscoveryEntry],
    pub port: &'a dyn AgentDispatchPort,
}

#[async_trait]
impl HostPublishClient for DiscoveryPublishClient<'_> {
    async fn publish(&self, event: ProducedEvent) -> Result<EventDeliveryOutcome> {
        let index = SubscriberIndex::new(self.entries);
        deliver_to_subscribers(&index, event, self.port).await
    }
}

/// Deliver one produced event to all subscription-matched subscribers.
pub async fn deliver_to_subscribers(
    index: &SubscriberIndex<'_>,
    event: ProducedEvent,
    port: &dyn AgentDispatchPort,
) -> Result<EventDeliveryOutcome> {
    let published = event.as_published_event();
    let targets = index.matching_routes(&published);

    if targets.is_empty() {
        return Ok(EventDeliveryOutcome {
            subscribers_matched: 0,
            subscribers_accepted: 0,
            acceptances: Vec::new(),
            failures: Vec::new(),
        });
    }

    let request = event.into_dispatch_request();
    let mut outcome = EventDeliveryOutcome {
        subscribers_matched: targets.len(),
        subscribers_accepted: 0,
        acceptances: Vec::new(),
        failures: Vec::new(),
    };

    for target in &targets {
        match port.dispatch(target, request.clone()).await {
            Ok(ack) if ack.accepted => {
                outcome.subscribers_accepted += 1;
                outcome.acceptances.push(SubscriberAcceptance {
                    agent_package: target.agent_package.to_string(),
                    agent_instance_id: target.agent_instance_id.to_string(),
                    detail: ack.detail.unwrap_or_else(|| "accepted".to_string()),
                });
            }
            Ok(ack) => {
                let detail = ack.detail.unwrap_or_else(|| "rejected".into());
                outcome.failures.push((
                    target.clone(),
                    SubscriberDeliveryFailure::Rejected { detail },
                ));
            }
            Err(err) => outcome.failures.push((
                target.clone(),
                SubscriberDeliveryFailure::from_dispatch_error(err),
            )),
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{ProducedEvent, matching_subscriber_routes};
    use crate::{
        AgentCard, AgentDiscoveryEntry, EventSchemaVersion, EventSubscription,
        event_subscription::{EventSourceKey, EventSourceKind, PublishedEvent},
    };

    fn entry(package: &str, subs: Vec<EventSubscription>) -> AgentDiscoveryEntry {
        AgentDiscoveryEntry {
            agent_package: package.into(),
            agent_instance_id: "default".into(),
            name: package.into(),
            version: "1".into(),
            agent_card: AgentCard {
                name: package.into(),
                version: "1".into(),
                content_hash: None,
                repository_version: None,
                agent_package: package.into(),
                agent_instance_id: "default".into(),
                tools: vec![],
                baml_functions: vec![],
                description: None,
                capabilities: vec![],
                tags: vec![],
                subscriptions: subs,
            },
        }
    }

    #[test]
    fn matching_uses_subscriptions_only() {
        let event = PublishedEvent {
            schema_version: EventSchemaVersion::parse("host.source-records.v1").unwrap(),
            source_kind: EventSourceKind::parse("clickup").unwrap(),
            source_key: EventSourceKey::parse("clickup:list-1").unwrap(),
        };
        let subscribed = entry(
            "coordinator-agent",
            vec![EventSubscription {
                schema_versions: vec![EventSchemaVersion::parse("host.source-records.v1").unwrap()],
                source_kinds: vec![EventSourceKind::parse("clickup").unwrap()],
                ..Default::default()
            }],
        );
        let wrong_schema = entry(
            "other-agent",
            vec![EventSubscription {
                schema_versions: vec![EventSchemaVersion::parse("other.v1").unwrap()],
                source_kinds: vec![EventSourceKind::parse("clickup").unwrap()],
                ..Default::default()
            }],
        );
        let routes = matching_subscriber_routes(&[subscribed, wrong_schema], &event);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].agent_package.as_str(), "coordinator-agent");
    }

    enum MockDispatchResponse {
        Accept,
        Reject(&'static str),
        Fail(&'static str),
    }

    struct MockDispatchPort {
        responses: std::collections::HashMap<String, MockDispatchResponse>,
    }

    #[async_trait::async_trait]
    impl super::AgentDispatchPort for MockDispatchPort {
        async fn dispatch(
            &self,
            target: &crate::AgentRouteKey,
            _request: crate::AgentDispatchRequest,
        ) -> crate::Result<crate::AgentDispatchAck> {
            let key = format!(
                "{}/{}",
                target.agent_package.as_str(),
                target.agent_instance_id.as_str()
            );
            match self.responses.get(&key) {
                Some(MockDispatchResponse::Accept) => Ok(crate::AgentDispatchAck {
                    accepted: true,
                    detail: None,
                    context_id: None,
                    task_id: None,
                    message_id: None,
                }),
                Some(MockDispatchResponse::Reject(detail)) => Ok(crate::AgentDispatchAck {
                    accepted: false,
                    detail: Some((*detail).to_string()),
                    context_id: None,
                    task_id: None,
                    message_id: None,
                }),
                Some(MockDispatchResponse::Fail(message)) => {
                    Err(crate::BamlRtError::InvalidArgument((*message).to_string()))
                }
                None => Err(crate::BamlRtError::InvalidArgument(format!(
                    "no mock response for {key}"
                ))),
            }
        }
    }

    fn produced_event() -> ProducedEvent {
        ProducedEvent::host_source_records(
            EventSourceKind::parse("clickup").unwrap(),
            EventSourceKey::parse("clickup:list-1").unwrap(),
            serde_json::json!({ "records": [] }),
            None,
            None,
        )
        .expect("test produced event")
    }

    fn subscriber_entry(package: &str) -> AgentDiscoveryEntry {
        entry(
            package,
            vec![EventSubscription {
                schema_versions: vec![EventSchemaVersion::parse("host.source-records.v1").unwrap()],
                source_kinds: vec![EventSourceKind::parse("clickup").unwrap()],
                ..Default::default()
            }],
        )
    }

    #[tokio::test]
    async fn deliver_to_subscribers_records_rejection_and_dispatch_errors() {
        use std::collections::HashMap;

        use super::{SubscriberDeliveryFailure, SubscriberIndex, deliver_to_subscribers};

        let entries = vec![
            subscriber_entry("accept-agent"),
            subscriber_entry("reject-agent"),
            subscriber_entry("error-agent"),
        ];
        let index = SubscriberIndex::new(&entries);
        let port = MockDispatchPort {
            responses: HashMap::from([
                ("accept-agent/default".into(), MockDispatchResponse::Accept),
                (
                    "reject-agent/default".into(),
                    MockDispatchResponse::Reject("not now"),
                ),
                (
                    "error-agent/default".into(),
                    MockDispatchResponse::Fail("boom"),
                ),
            ]),
        };

        let outcome = deliver_to_subscribers(&index, produced_event(), &port)
            .await
            .expect("delivery should aggregate partial failures");

        assert_eq!(outcome.subscribers_matched, 3);
        assert_eq!(outcome.subscribers_accepted, 1);
        assert_eq!(outcome.failures.len(), 2);
        assert!(outcome.failures.iter().any(|(route, failure)| {
            route.agent_package.as_str() == "reject-agent"
                && matches!(
                    failure,
                    SubscriberDeliveryFailure::Rejected { detail } if detail == "not now"
                )
        }));
        assert!(outcome.failures.iter().any(|(route, failure)| {
            route.agent_package.as_str() == "error-agent"
                && matches!(
                    failure,
                    SubscriberDeliveryFailure::Dispatch { detail } if detail.contains("boom")
                )
        }));
    }
}
