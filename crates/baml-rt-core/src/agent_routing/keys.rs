//! Typed route identity: package name, instance id, and route key parsing from A2A requests.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BamlRtError, Result};

fn is_valid_identifier(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Strongly-typed agent package identifier (e.g. manifest name).
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentPackageName(String);

impl AgentPackageName {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let raw = value.as_ref();
        if raw.trim().is_empty() || raw != raw.trim() || !is_valid_identifier(raw) {
            return None;
        }
        Some(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentPackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for AgentPackageName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentPackageName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Strongly-typed agent instance identifier.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentInstanceId(String);

impl AgentInstanceId {
    pub const DEFAULT: &'static str = "default";

    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let raw = value.as_ref();
        if raw.trim().is_empty() || raw != raw.trim() || !is_valid_identifier(raw) {
            return None;
        }
        Some(Self(raw.to_string()))
    }

    pub fn default_id() -> Self {
        Self(Self::DEFAULT.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AgentInstanceId {
    fn default() -> Self {
        Self::default_id()
    }
}

impl fmt::Display for AgentInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for AgentInstanceId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentInstanceId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Route key for an agent instance: agent_package (e.g. manifest name) + agent_instance_id.
/// Used in HTTP paths: `/agents/{agent_package}/{agent_instance_id}/...`
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentRouteKey {
    pub agent_package: AgentPackageName,
    pub agent_instance_id: AgentInstanceId,
}

impl AgentRouteKey {
    pub fn new(agent_package: AgentPackageName, agent_instance_id: AgentInstanceId) -> Self {
        Self {
            agent_package,
            agent_instance_id,
        }
    }
}

/// Extract route key from a JSON-RPC A2A request (params.metadata.target from system/internal_a2a).
/// Centralizes the protocol so all consumers use the same parsing.
pub fn route_key_from_request(request: impl AsRef<Value>) -> Result<AgentRouteKey> {
    let request = request.as_ref();
    let params = request
        .get("params")
        .and_then(|p| p.as_object())
        .ok_or_else(|| BamlRtError::InvalidArgument("params must be an object".to_string()))?;
    let meta = params
        .get("metadata")
        .and_then(|m| m.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata required for routing".to_string())
        })?;
    let target = meta
        .get("target")
        .and_then(|t| t.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata.target required".to_string())
        })?;
    let agent_package_str = target
        .get("agent_package")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "params.metadata.target.agent_package required".to_string(),
            )
        })?;
    let agent_instance_id_str = target
        .get("agent_instance_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let agent_package = AgentPackageName::parse(agent_package_str).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!("invalid agent_package '{agent_package_str}'"))
    })?;
    let agent_instance_id = AgentInstanceId::parse(agent_instance_id_str).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "invalid agent_instance_id '{agent_instance_id_str}'"
        ))
    })?;
    Ok(AgentRouteKey::new(agent_package, agent_instance_id))
}

#[cfg(test)]
mod tests {
    use super::{AgentInstanceId, AgentPackageName};

    #[test]
    fn package_name_parse_rejects_invalid_characters() {
        assert!(AgentPackageName::parse("coordinator-agent").is_some());
        assert!(AgentPackageName::parse("notion_agent").is_some());
        assert!(AgentPackageName::parse("bad/name").is_none());
        assert!(AgentPackageName::parse("bad name").is_none());
        assert!(AgentPackageName::parse(" coordinator-agent ").is_none());
        assert!(AgentPackageName::parse("   ").is_none());
    }

    #[test]
    fn instance_id_parse_rejects_empty_and_invalid() {
        assert!(AgentInstanceId::parse("default").is_some());
        assert!(AgentInstanceId::parse("").is_none());
        assert!(AgentInstanceId::parse(" staging ").is_none());
        assert!(AgentInstanceId::parse("..\0").is_none());
    }
}
