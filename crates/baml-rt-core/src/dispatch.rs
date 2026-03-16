use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

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
    use super::AgentDispatchRoutingKey;

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
