use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Structured transport metadata attached to a host-to-agent dispatch.
///
/// Object-shaped by design: `messages` carries arbitrary event payloads,
/// while `metadata` carries structured delivery context (source identifier,
/// schema version, content type, etc.).  Using a `Map` rather than a free-form
/// `Value` enforces this separation at the type level.
pub type DispatchMetadata = serde_json::Map<String, Value>;

use crate::{
    EventSchemaVersion,
    ids::{ContextId, TaskId},
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
    #[serde(default)]
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
    /// Structured transport metadata (source, schema version, content type, etc.).
    ///
    /// Object-shaped: use `messages` for arbitrary event payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<DispatchMetadata>,
}

/// Buffered acknowledgement for deterministic host delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDispatchAck {
    /// True when the receiving agent accepted the delivery.
    pub accepted: bool,
    /// Optional operator-facing detail string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentDispatchRequest, AgentDispatchRoutingKey, DispatchMetadata};

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

    #[test]
    fn metadata_round_trips_as_structured_object() {
        let mut meta = DispatchMetadata::new();
        meta.insert("source".into(), json!("baml-task-daemon"));
        meta.insert(
            "content_type".into(),
            json!("application/vnd.baml.interpretation+json"),
        );

        let request = AgentDispatchRequest {
            routing_key: AgentDispatchRoutingKey::parse("slack:intake").unwrap(),
            message_type: crate::EventSchemaVersion::parse("test.v1").unwrap(),
            messages: vec![json!({"payload": "data"})],
            context_id: None,
            task_id: None,
            message_id: None,
            metadata: Some(meta),
        };

        let json_str = serde_json::to_string(&request).expect("serialize");
        let parsed: AgentDispatchRequest = serde_json::from_str(&json_str).expect("deserialize");

        let meta = parsed.metadata.as_ref().expect("metadata present");
        assert_eq!(
            meta.get("source").and_then(|v| v.as_str()),
            Some("baml-task-daemon")
        );
        assert_eq!(
            meta.get("content_type").and_then(|v| v.as_str()),
            Some("application/vnd.baml.interpretation+json")
        );
    }

    #[test]
    fn metadata_rejects_non_object_json() {
        let raw = r#"{
            "routing_key": "slack:intake",
            "message_type": "test.v1",
            "metadata": "not-an-object"
        }"#;
        assert!(
            serde_json::from_str::<AgentDispatchRequest>(raw).is_err(),
            "string metadata must be rejected"
        );

        let raw_array = r#"{
            "routing_key": "slack:intake",
            "message_type": "test.v1",
            "metadata": [1, 2, 3]
        }"#;
        assert!(
            serde_json::from_str::<AgentDispatchRequest>(raw_array).is_err(),
            "array metadata must be rejected"
        );
    }

    #[test]
    fn serializes_empty_messages_field() {
        let request = AgentDispatchRequest {
            routing_key: AgentDispatchRoutingKey::parse("slack:intake").unwrap(),
            message_type: crate::EventSchemaVersion::parse("test.v1").unwrap(),
            messages: Vec::new(),
            context_id: None,
            task_id: None,
            message_id: None,
            metadata: None,
        };

        let json_value = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json_value.get("messages"), Some(&json!([])));
    }
}
