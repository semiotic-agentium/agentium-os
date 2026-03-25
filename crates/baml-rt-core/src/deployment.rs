//! Deployment lifecycle contracts shared across runtime components.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeploymentContentHash(String);

impl DeploymentContentHash {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
#[async_trait(?Send)]
pub trait DeploymentManager: Send + Sync {
    async fn deploy_by_hash(&self, content_hash: &DeploymentContentHash) -> Result<DeployResult>;
    async fn undeploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<UndeployResult>;
    async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>>;
}
