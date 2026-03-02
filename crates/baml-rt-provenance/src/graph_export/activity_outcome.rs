//! Activity outcome for graph nodes.
//!
//! Inferred from (1) activity having an end time and (2) outcome.
//! Parses `a2a:activity_outcome` only.

use std::collections::HashMap;

use serde_json::Value;

use crate::vocabulary::a2a;

/// Strong-typed activity outcome for graph nodes.
/// Replaces ad-hoc string/bool checks across display, simplify, and sequence rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeActivityOutcome {
    InProgress,
    Success,
    Failed,
}

impl NodeActivityOutcome {
    /// Parse from node properties. Checks `a2a:activity_outcome` only.
    pub fn from_props(props: &HashMap<String, Value>) -> Option<Self> {
        props
            .get(a2a::ACTIVITY_OUTCOME)
            .and_then(|v| v.as_str())
            .and_then(|v| match v {
                "Success" => Some(Self::Success),
                "Failed" => Some(Self::Failed),
                "InProgress" => Some(Self::InProgress),
                _ => None,
            })
    }

    pub fn is_completed(self) -> bool {
        matches!(self, Self::Success | Self::Failed)
    }

    /// Suffix for display names (✅ / ❌).
    pub fn display_suffix(self) -> &'static str {
        match self {
            Self::Success => " ✅",
            Self::Failed => " ❌",
            Self::InProgress => "",
        }
    }
}
