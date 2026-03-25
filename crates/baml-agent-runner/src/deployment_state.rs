use std::{path::Path, sync::Arc};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use surrealdb::{
    Surreal,
    engine::local::{Db, SurrealKv},
};
#[cfg(test)]
use surrealdb::engine::local::Mem;

const NS: &str = "baml";
const DB_NAME: &str = "runner_state";
const TBL_DEPLOYMENTS: &str = "deployments";

const SCHEMA_QUERIES: &[&str] = &[
    "DEFINE TABLE IF NOT EXISTS deployments SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS content_hash ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS agent_name ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS deployed_at ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS status ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS last_error ON deployments TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS last_attempt_at ON deployments TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS failure_count ON deployments TYPE int",
    "DEFINE INDEX IF NOT EXISTS idx_deploy_content_hash ON deployments FIELDS content_hash UNIQUE",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentStatus {
    Active,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub content_hash: String,
    pub agent_name: String,
    pub deployed_at: String,
    pub status: DeploymentStatus,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<String>,
    pub failure_count: u32,
}

pub struct DeploymentStateStore {
    db: Arc<Surreal<Db>>,
}

impl DeploymentStateStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path.as_ref().to_string_lossy().as_ref())
            .await
            .map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await.map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<()> {
        for stmt in SCHEMA_QUERIES {
            self.db.query(*stmt).await.map_err(to_write_err)?;
        }
        Ok(())
    }

    pub async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT content_hash,agent_name,deployed_at,status,last_error,last_attempt_at,failure_count FROM {TBL_DEPLOYMENTS}"
            ))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

impl TryFrom<serde_json::Value> for DeploymentRecord {
    type Error = BamlRtError;

    fn try_from(row: serde_json::Value) -> std::result::Result<Self, Self::Error> {
        let content_hash = row
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("deployments.content_hash missing".into())
            })?
            .to_string();
        let agent_name = row
            .get("agent_name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| BamlRtError::InvalidArgument("deployments.agent_name missing".into()))?
            .to_string();
        let deployed_at = row
            .get("deployed_at")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("deployments.deployed_at missing".into())
            })?
            .to_string();
        let status = match row.get("status").and_then(serde_json::Value::as_str) {
            Some("active") => DeploymentStatus::Active,
            Some("failed") => DeploymentStatus::Failed,
            _ => {
                return Err(BamlRtError::InvalidArgument(
                    "deployments.status invalid".into(),
                ));
            }
        };
        let last_error = row
            .get("last_error")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let last_attempt_at = row
            .get("last_attempt_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let failure_count = row
            .get("failure_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;

        Ok(Self {
            content_hash,
            agent_name,
            deployed_at,
            status,
            last_error,
            last_attempt_at,
            failure_count,
        })
    }
}

fn to_write_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::InvalidArgument(format!("runner state write failed: {err}"))
}

fn to_read_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::InvalidArgument(format!("runner state read failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_and_lists_empty() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let records = store.list_deployments().await.unwrap();
        assert!(records.is_empty());
    }
}
