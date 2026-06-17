// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, fmt};

use baml_rt_id::{
    ProvActivitySemantics, ProvAgentSemantics, ProvConstantActivitySemantics,
    ProvConstantAgentSemantics, ProvConstantEntitySemantics, ProvConstantIdTemplate,
    ProvDerivedActivitySemantics, ProvDerivedAgentSemantics, ProvDerivedEntitySemantics,
    ProvDerivedIdTemplate, ProvEntitySemantics,
};
use serde::{Deserialize, Serialize};

macro_rules! define_prov_id_type {
    ($(#[$doc:meta])* $name:ident, $sem_trait:ident, $derived_trait:ident, $const_trait:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn derived<S>(input: S::Input<'_>) -> Self
            where
                S: $sem_trait + $derived_trait + ProvDerivedIdTemplate,
            {
                Self(S::build(input).into_string())
            }

            pub fn constant<S>() -> Self
            where
                S: $sem_trait + $const_trait + ProvConstantIdTemplate,
            {
                Self(S::build().as_str().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_prov_id_type!(
    /// Provenance entity identifier.
    ProvEntityId,
    ProvEntitySemantics,
    ProvDerivedEntitySemantics,
    ProvConstantEntitySemantics
);

impl ProvEntityId {
    /// Construct from a write-time node ID for cross-event edge references.
    ///
    /// Used when the normalizer needs to reference a node created in a different
    /// normalization pass (e.g. CITED edges from LlmCall to a SessionStep entity
    /// that was normalized from a prior event). The caller must construct the ID
    /// using the same semantic type that created the original node.
    ///
    /// **Write-time only.** Must never be used on the read path.
    pub(crate) fn from_write_time_node_id(node_id: impl Into<String>) -> Self {
        Self(node_id.into())
    }
}

#[cfg(test)]
impl ProvEntityId {
    pub fn test_only(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

define_prov_id_type!(
    /// Provenance activity identifier.
    ProvActivityId,
    ProvActivitySemantics,
    ProvDerivedActivitySemantics,
    ProvConstantActivitySemantics
);

#[cfg(test)]
impl ProvActivityId {
    pub fn test_only(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

impl ProvActivityId {
    /// Construct from a write-time node ID for cross-event edge references.
    pub(crate) fn from_write_time_node_id(node_id: impl Into<String>) -> Self {
        Self(node_id.into())
    }
}

define_prov_id_type!(
    /// Provenance agent identifier.
    ProvAgentId,
    ProvAgentSemantics,
    ProvDerivedAgentSemantics,
    ProvConstantAgentSemantics
);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    #[serde(rename = "prov:type", skip_serializing_if = "Option::is_none")]
    pub prov_type: Option<String>,
    #[serde(flatten)]
    pub attributes: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    #[serde(rename = "prov:startTime", skip_serializing_if = "Option::is_none")]
    pub start_time_ms: Option<u64>,
    #[serde(rename = "prov:endTime", skip_serializing_if = "Option::is_none")]
    pub end_time_ms: Option<u64>,
    #[serde(rename = "prov:type", skip_serializing_if = "Option::is_none")]
    pub prov_type: Option<String>,
    #[serde(flatten)]
    pub attributes: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    #[serde(rename = "prov:type", skip_serializing_if = "Option::is_none")]
    pub prov_type: Option<String>,
    #[serde(flatten)]
    pub attributes: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProvNodeRef {
    Entity(ProvEntityId),
    Activity(ProvActivityId),
    Agent(ProvAgentId),
}

impl ProvNodeRef {
    pub fn label(&self) -> &'static str {
        match self {
            ProvNodeRef::Entity(_) => "ProvEntity",
            ProvNodeRef::Activity(_) => "ProvActivity",
            ProvNodeRef::Agent(_) => "ProvAgent",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            ProvNodeRef::Entity(id) => id.as_str(),
            ProvNodeRef::Activity(id) => id.as_str(),
            ProvNodeRef::Agent(id) => id.as_str(),
        }
    }
}

impl From<ProvEntityId> for ProvNodeRef {
    fn from(value: ProvEntityId) -> Self {
        ProvNodeRef::Entity(value)
    }
}

impl From<ProvAgentId> for ProvNodeRef {
    fn from(value: ProvAgentId) -> Self {
        ProvNodeRef::Agent(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Used {
    #[serde(rename = "prov:activity")]
    pub activity: ProvActivityId,
    #[serde(rename = "prov:entity")]
    pub entity: ProvEntityId,
    #[serde(rename = "prov:role", skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasAssociatedWith {
    #[serde(rename = "prov:activity")]
    pub activity: ProvActivityId,
    #[serde(rename = "prov:agent")]
    pub agent: ProvAgentId,
    #[serde(rename = "prov:role", skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasGeneratedBy {
    #[serde(rename = "prov:entity")]
    pub entity: ProvNodeRef,
    #[serde(rename = "prov:activity")]
    pub activity: ProvActivityId,
    #[serde(rename = "prov:time", skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QualifiedGeneration {
    #[serde(rename = "prov:entity")]
    pub entity: ProvNodeRef,
    #[serde(rename = "prov:activity")]
    pub activity: ProvActivityId,
    #[serde(rename = "prov:time", skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WasDerivedFrom {
    #[serde(rename = "prov:generatedEntity")]
    pub generated_entity: ProvEntityId,
    #[serde(rename = "prov:usedEntity")]
    pub used_entity: ProvEntityId,
    #[serde(rename = "prov:activity", skip_serializing_if = "Option::is_none")]
    pub activity: Option<ProvActivityId>,
    #[serde(rename = "prov:type", skip_serializing_if = "Option::is_none")]
    pub prov_type: Option<String>,
}
