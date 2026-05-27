//! Deterministic activity anchors and message ids for host ingress transcript rows.
//!
//! Identity is content-addressable via [`HostIngressTranscriptId`] (`DerivedId::from_parts`).

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, MessageId, UuidId};
use baml_rt_id::{DerivedId, ProvDerivedIdTemplate};
use uuid::Uuid;

use crate::{
    events::{ProvEvent, ProvEventData},
    host_ingress_types::{
        HostIngressDispatchOutcomeKey, HostIngressKind, HostIngressPollKey, HostIngressSourceRef,
    },
    id_semantics::{HostIngressTranscriptId, HostIngressTranscriptInput},
};

/// Wire kind for derived poll-batch user ingress anchors.
pub const INGRESS_POLL_USER_KIND: &str = "ingress_poll_user";
/// Wire kind for derived unit-scoped user ingress anchors.
pub const INGRESS_UNIT_USER_KIND: &str = "ingress_unit_user";

fn derived_transcript_id_str(context_id: &ContextId, kind: &str, components: &[&str]) -> DerivedId {
    HostIngressTranscriptId::build(HostIngressTranscriptInput {
        context_id,
        kind,
        components,
    })
}

fn derived_transcript_id(
    context_id: &ContextId,
    kind: HostIngressKind,
    components: &[&str],
) -> DerivedId {
    derived_transcript_id_str(context_id, kind.as_wire_str(), components)
}

#[must_use]
pub fn activity_anchor_for_poll(key: &HostIngressPollKey) -> ActivityAnchorId {
    let derived = derived_transcript_id(
        &key.context_id,
        HostIngressKind::SourcePollRecorded,
        &[
            key.source_kind.as_str(),
            key.source_key.as_str(),
            key.source_cursor.as_str(),
        ],
    );
    ActivityAnchorId::from(derived.into_string())
}

#[must_use]
pub fn message_id_for_poll(key: &HostIngressPollKey) -> MessageId {
    let derived = derived_transcript_id(
        &key.context_id,
        HostIngressKind::SourcePollRecorded,
        &[
            key.source_kind.as_str(),
            key.source_key.as_str(),
            key.source_cursor.as_str(),
        ],
    );
    MessageId::from_derived(derived)
}

#[must_use]
pub fn activity_anchor_for_ingress_poll_user(
    context_id: &ContextId,
    batch_message_id: &str,
) -> ActivityAnchorId {
    let derived =
        derived_transcript_id_str(context_id, INGRESS_POLL_USER_KIND, &[batch_message_id]);
    ActivityAnchorId::from(derived.into_string())
}

#[must_use]
pub fn activity_anchor_for_ingress_unit_user(
    context_id: &ContextId,
    unit_key: &str,
) -> ActivityAnchorId {
    let derived = derived_transcript_id_str(context_id, INGRESS_UNIT_USER_KIND, &[unit_key]);
    ActivityAnchorId::from(derived.into_string())
}

/// Deterministic message id for host ingress operational transcript rows.
#[must_use]
pub fn host_ingress_message_id(event: &ProvEvent) -> MessageId {
    let context_id = event.context_id();
    match event.data() {
        ProvEventData::HostSourcePollRecorded {
            source_kind,
            source_key,
            source_cursor,
            ..
        } => message_id_for_poll(&HostIngressPollKey {
            context_id: context_id.clone(),
            source_kind: source_kind.clone(),
            source_key: source_key.clone(),
            source_cursor: source_cursor.clone(),
        }),
        ProvEventData::HostDispatchAccepted {
            routing_key,
            target_package,
            target_instance,
            source_kind,
            source_key,
            ..
        } => message_id_for_dispatch_outcome(&dispatch_outcome_key(
            context_id.clone(),
            HostIngressKind::DispatchAccepted,
            routing_key,
            target_package.clone(),
            target_instance.clone(),
            HostIngressSourceRef::from_fields(source_kind, source_key),
        )),
        ProvEventData::HostDispatchRejected {
            routing_key,
            target_package,
            target_instance,
            source_kind,
            source_key,
            failure_kind,
            ..
        } => message_id_for_dispatch_outcome(&dispatch_outcome_key(
            context_id.clone(),
            HostIngressKind::from_dispatch_failure(*failure_kind),
            routing_key,
            target_package.clone(),
            target_instance.clone(),
            HostIngressSourceRef::from_fields(source_kind, source_key),
        )),
        other => panic!("host_ingress_message_id called for non-host-ingress event: {other:?}"),
    }
}

#[must_use]
pub fn activity_anchor_for_dispatch_outcome(
    key: &HostIngressDispatchOutcomeKey,
) -> ActivityAnchorId {
    let derived = derived_transcript_id(
        &key.context_id,
        key.kind,
        &[
            key.routing_key.as_str(),
            key.target_package.as_str(),
            key.target_instance.as_str(),
            key.source.key_wire(),
        ],
    );
    ActivityAnchorId::from(derived.into_string())
}

#[must_use]
pub fn message_id_for_dispatch_outcome(key: &HostIngressDispatchOutcomeKey) -> MessageId {
    let derived = derived_transcript_id(
        &key.context_id,
        key.kind,
        &[
            key.routing_key.as_str(),
            key.target_package.as_str(),
            key.target_instance.as_str(),
            key.source.key_wire(),
        ],
    );
    MessageId::from_derived(derived)
}

#[must_use]
pub fn dispatch_outcome_key(
    context_id: ContextId,
    kind: HostIngressKind,
    routing_key: impl AsRef<str>,
    target_package: impl Into<String>,
    target_instance: impl Into<String>,
    source: HostIngressSourceRef,
) -> HostIngressDispatchOutcomeKey {
    let routing = routing_key.as_ref();
    HostIngressDispatchOutcomeKey {
        context_id,
        kind,
        routing_key: baml_rt_core::AgentDispatchRoutingKey::parse(routing).unwrap_or_else(|| {
            panic!("host ingress dispatch outcome requires non-empty routing key, got '{routing}'")
        }),
        target_package: target_package.into(),
        target_instance: target_instance.into(),
        source,
    }
}

/// Stable route-scoped agent id for `HOST_DISPATCH_TARGET` edges (not a live runtime UUID).
#[must_use]
pub fn route_target_agent_id(target_package: &str, target_instance: &str) -> AgentId {
    let name = format!("route:{target_package}/{target_instance}");
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes());
    AgentId::from_uuid(UuidId::new(id))
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::ContextId;

    use super::*;
    use crate::host_ingress_types::HostIngressSourceRef;

    #[test]
    fn dispatch_reject_identity_is_stable_and_distinct_from_transport() {
        let ctx = ContextId::new(1, 2);
        let source = HostIngressSourceRef::from_fields("slack", "slack:C1");
        let reject = dispatch_outcome_key(
            ctx.clone(),
            HostIngressKind::DispatchRejected,
            "slack:intake",
            "slack-agent",
            "default",
            source.clone(),
        );
        let reject_again = dispatch_outcome_key(
            ctx.clone(),
            HostIngressKind::DispatchRejected,
            "slack:intake",
            "slack-agent",
            "default",
            source.clone(),
        );
        assert_eq!(
            activity_anchor_for_dispatch_outcome(&reject),
            activity_anchor_for_dispatch_outcome(&reject_again)
        );
        let transport = dispatch_outcome_key(
            ctx,
            HostIngressKind::DispatchTransportError,
            "slack:intake",
            "slack-agent",
            "default",
            source,
        );
        assert_ne!(
            activity_anchor_for_dispatch_outcome(&reject),
            activity_anchor_for_dispatch_outcome(&transport)
        );
    }

    #[test]
    fn unspecified_source_is_explicit_in_identity() {
        let ctx = ContextId::new(3, 4);
        let key = dispatch_outcome_key(
            ctx,
            HostIngressKind::DispatchRejected,
            "event:intake",
            "pkg",
            "default",
            HostIngressSourceRef::Unspecified,
        );
        let anchor = activity_anchor_for_dispatch_outcome(&key);
        assert!(
            anchor
                .as_str()
                .contains(HostIngressSourceRef::UNSPECIFIED_KEY)
        );
    }
}
