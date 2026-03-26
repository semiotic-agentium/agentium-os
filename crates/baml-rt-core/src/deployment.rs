//! Deployment lifecycle contracts shared across runtime components.

use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeploymentContentHash(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid deployment content hash: expected lowercase sha256 hex (64 chars)")]
pub struct DeploymentContentHashParseError;

impl DeploymentContentHash {
    pub fn new(
        value: impl Into<String>,
    ) -> std::result::Result<Self, DeploymentContentHashParseError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DeploymentContentHash {
    type Err = DeploymentContentHashParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return Ok(Self(s.to_string()));
        }
        Err(DeploymentContentHashParseError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentStatus {
    Active,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub content_hash: DeploymentContentHash,
    pub agent_name: String,
    pub deployed_at: String,
    pub status: DeploymentStatus,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<String>,
    pub failure_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployResult {
    pub already_deployed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndeployResult {
    pub removed: bool,
}

/// Runner-side deployment management surface.
///
/// `?Send` is intentional: deployment boot currently traverses runtime internals that
/// are not `Send` across await points, so implementors are local-executor bound.
#[async_trait(?Send)]
pub trait DeploymentManager: Send + Sync {
    async fn deploy_by_hash(&self, content_hash: &DeploymentContentHash) -> Result<DeployResult>;
    async fn undeploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<UndeployResult>;
    async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>>;
}
