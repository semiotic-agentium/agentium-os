// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    EventSchemaVersion,
    context::{InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId},
};

/// Strongly-typed route label for deterministic host-to-agent dispatch.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AgentDispatchRoutingKey(String);

impl AgentDispatchRoutingKey {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let trimmed = value.as_ref().trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentDispatchRoutingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentDispatchRoutingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw)
            .ok_or_else(|| serde::de::Error::custom("invalid agent dispatch routing key"))
    }
}

/// Deterministic host-to-agent delivery request for non-conversational workloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentDispatchRequest {
    /// Stable route label for the receiving entrypoint (for example `slack:intake`).
    pub routing_key: AgentDispatchRoutingKey,
    /// Message family / schema identifier for the payload batch.
    pub message_type: EventSchemaVersion,
    /// Opaque payloads delivered to the agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Value>,
    /// Optional existing context to continue under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    /// Optional existing task to continue under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Optional caller-supplied message id for provenance continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Optional transport metadata for the receiving agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// JSON keys on [`AgentDispatchRequest::metadata`] and [`crate::ProducedEvent::metadata`] that
/// carry the scheduling A2A scope for callback delivery deferral and provenance linking.
pub const DISPATCH_METADATA_SCHEDULING_CONTEXT_ID: &str = "schedulingContextId";
/// See [`DISPATCH_METADATA_SCHEDULING_CONTEXT_ID`].
pub const DISPATCH_METADATA_SCHEDULING_TASK_ID: &str = "schedulingTaskId";

/// Parse scheduling scope from dispatch metadata written by the callback event producer.
pub fn scheduling_scope_from_dispatch_metadata(meta: &Value) -> Option<(ContextId, TaskId)> {
    let sched_ctx = meta
        .get(DISPATCH_METADATA_SCHEDULING_CONTEXT_ID)?
        .as_str()?;
    let sched_task = meta.get(DISPATCH_METADATA_SCHEDULING_TASK_ID)?.as_str()?;
    Some((
        ContextId::from(sched_ctx),
        TaskId::from_external(ExternalId::new(sched_task.to_string())),
    ))
}

/// `true` when minted dispatch scope differs from the scheduling A2A turn (detached continuation).
pub fn callback_scheduling_scopes_differ_from_dispatch(
    scheduling_context_id: &ContextId,
    scheduling_task_id: &TaskId,
    dispatch_context_id: &ContextId,
    dispatch_task_id: &TaskId,
) -> bool {
    scheduling_context_id != dispatch_context_id || scheduling_task_id != dispatch_task_id
}

/// Transport metadata for host-published events (no prompt — meaning lives in `messages[]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchMetadata {
    /// Emitting component (for example `baml-task-daemon` or `support/slack`).
    pub producer: String,
    /// MIME type of the first `messages[]` element.
    pub content_type: String,
    /// Wire schema version echoed for operators.
    pub event_schema_version: String,
}

impl DispatchMetadata {
    pub fn task_daemon_publish(schema_version: &str) -> Self {
        Self {
            producer: "baml-task-daemon".into(),
            content_type: "application/json".into(),
            event_schema_version: schema_version.to_string(),
        }
    }

    pub fn into_value(self) -> Value {
        serde_json::to_value(self).expect("DispatchMetadata serializes to JSON")
    }
}

/// Buffered acknowledgement for deterministic host delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDispatchAck {
    /// True when the receiving agent accepted the delivery.
    pub accepted: bool,
    /// Optional operator-facing detail string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Invocation context used for this dispatch (when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

impl AgentDispatchAck {
    /// Attach resolved runtime scope ids for operator observation UIs.
    pub fn with_resolved_scope(mut self, scope: &crate::context::RuntimeScope) -> Self {
        self.context_id = Some(scope.context_id().to_string());
        self.message_id = Some(scope.message_id().to_string());
        self.task_id = scope.task_id_opt().map(|t| t.to_string());
        self
    }
}

/// Build the invocation scope for a host [`AgentDispatchRequest`].
///
/// When both `context_id` and `message_id` are present, scope matches the request (task-scoped if
/// `task_id` is set). Otherwise uses a synthetic message scope (same family as ad-hoc tests/CLI).
pub fn invocation_scope_for_agent_dispatch(
    agent_id: AgentId,
    request: &AgentDispatchRequest,
) -> InvocationScope {
    match (&request.context_id, &request.message_id) {
        (Some(context_id), Some(message_id)) => {
            let message_id = MessageId::from(message_id.as_str());
            if let Some(task_id) = &request.task_id {
                InvocationScope::new(RuntimeScope::task_scope(
                    context_id.clone(),
                    agent_id,
                    message_id,
                    task_id.clone(),
                ))
            } else {
                InvocationScope::new(RuntimeScope::message_scope(
                    context_id.clone(),
                    agent_id,
                    message_id,
                ))
            }
        }
        _ => InvocationScope::synthetic_message(agent_id),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AgentDispatchAck, AgentDispatchRoutingKey, DISPATCH_METADATA_SCHEDULING_CONTEXT_ID,
        DISPATCH_METADATA_SCHEDULING_TASK_ID, callback_scheduling_scopes_differ_from_dispatch,
        scheduling_scope_from_dispatch_metadata,
    };
    use crate::ids::{ContextId, TaskId};

    #[test]
    fn with_resolved_scope_attaches_runtime_ids() {
        use crate::{
            context::RuntimeScope,
            ids::{AgentId, ContextId, MessageId, TaskId, UuidId},
        };

        let context_id = ContextId::new(1, 2);
        let message_id = MessageId::from("msg-1");
        let task_id = TaskId::from_external(crate::ids::ExternalId::new("task-1".to_string()));
        let scope = RuntimeScope::task_scope(
            context_id.clone(),
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000000").unwrap()),
            message_id.clone(),
            task_id.clone(),
        );
        let ack = AgentDispatchAck {
            accepted: true,
            detail: None,
            context_id: None,
            task_id: None,
            message_id: None,
        }
        .with_resolved_scope(&scope);
        assert_eq!(ack.context_id.as_deref(), Some(context_id.as_str()));
        assert_eq!(ack.message_id.as_deref(), Some(message_id.as_str()));
        assert_eq!(ack.task_id.as_deref(), Some(task_id.as_str()));
    }

    #[test]
    fn scheduling_scope_from_dispatch_metadata_parses_callback_keys() {
        let meta = json!({
            DISPATCH_METADATA_SCHEDULING_CONTEXT_ID: "ctx-10-20",
            DISPATCH_METADATA_SCHEDULING_TASK_ID: "task-parent",
        });
        let (ctx, task) = scheduling_scope_from_dispatch_metadata(&meta).expect("parse");
        assert_eq!(ctx.as_str(), "ctx-10-20");
        assert_eq!(task.as_str(), "task-parent");
    }

    #[test]
    fn callback_scheduling_scopes_differ_from_dispatch_detects_detached() {
        let sc = ContextId::new(1, 1);
        let st = TaskId::from_external(crate::ids::ExternalId::new("a"));
        let dc = ContextId::new(2, 2);
        let dt = TaskId::from_external(crate::ids::ExternalId::new("b"));
        assert!(callback_scheduling_scopes_differ_from_dispatch(
            &sc, &st, &dc, &dt
        ));
        assert!(!callback_scheduling_scopes_differ_from_dispatch(
            &sc, &st, &sc, &st
        ));
    }

    #[test]
    fn routing_key_parse_rejects_blank_values() {
        assert!(AgentDispatchRoutingKey::parse("").is_none());
        assert!(AgentDispatchRoutingKey::parse("   ").is_none());
    }

    #[test]
    fn routing_key_deserialize_trims_whitespace() {
        let key: AgentDispatchRoutingKey =
            serde_json::from_str("\"  slack:intake  \"").expect("routing key should deserialize");
        assert_eq!(key.as_str(), "slack:intake");
    }
}
