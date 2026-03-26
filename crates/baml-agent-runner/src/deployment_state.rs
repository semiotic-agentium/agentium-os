use std::{path::Path, sync::Arc};

use baml_rt_core::{
    BamlRtError, DeploymentContentHash, DeploymentRecord, DeploymentStatus, Result,
};
use serde::Deserialize;
#[cfg(test)]
use surrealdb::engine::local::Mem;
use surrealdb::{
    Surreal,
    engine::local::{Db, SurrealKv},
};

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

    pub async fn save_deployment(&self, record: &DeploymentRecord) -> Result<()> {
        let status_value = serde_json::to_value(&record.status).map_err(|e| {
            BamlRtError::InvalidArgument(format!("failed to serialize deployment status: {e}"))
        })?;
        let status = status_value
            .as_str()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "serialized deployment status is not a string".to_string(),
                )
            })?
            .to_string();

        self.db
            .query(format!(
                "UPSERT {TBL_DEPLOYMENTS} SET \
                    content_hash = $content_hash, \
                    agent_name = $agent_name, \
                    deployed_at = $deployed_at, \
                    status = $status, \
                    last_error = $last_error, \
                    last_attempt_at = $last_attempt_at, \
                    failure_count = $failure_count \
                 WHERE content_hash = $content_hash"
            ))
            .bind(("content_hash", record.content_hash.as_str().to_string()))
            .bind(("agent_name", record.agent_name.clone()))
            .bind(("deployed_at", record.deployed_at.clone()))
            .bind(("status", status))
            .bind(("last_error", record.last_error.clone()))
            .bind(("last_attempt_at", record.last_attempt_at.clone()))
            .bind(("failure_count", record.failure_count as i64))
            .await
            .map_err(to_write_err)?;
        Ok(())
    }

    pub async fn remove_deployment(&self, content_hash: &DeploymentContentHash) -> Result<bool> {
        let mut resp = self
            .db
            .query(format!(
                "DELETE FROM {TBL_DEPLOYMENTS} WHERE content_hash = $content_hash RETURN BEFORE"
            ))
            .bind(("content_hash", content_hash.as_str().to_string()))
            .await
            .map_err(to_write_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        if !rows.is_empty() {
            return Ok(true);
        }
        let rows_alt: Vec<serde_json::Value> = resp.take(1).map_err(to_read_err)?;
        Ok(!rows_alt.is_empty())
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
        rows.into_iter().map(parse_deployment_row).collect()
    }
}

fn parse_deployment_row(row: serde_json::Value) -> Result<DeploymentRecord> {
    #[derive(Deserialize)]
    struct DeploymentRow {
        content_hash: String,
        agent_name: String,
        deployed_at: String,
        status: DeploymentStatus,
        #[serde(default)]
        last_error: Option<String>,
        #[serde(default)]
        last_attempt_at: Option<String>,
        #[serde(default)]
        failure_count: u32,
    }

    let parsed: DeploymentRow = serde_json::from_value(row).map_err(|e| {
        BamlRtError::InvalidArgument(format!("invalid deployment row from state DB: {e}"))
    })?;

    Ok(DeploymentRecord {
        content_hash: parsed.content_hash.parse().map_err(|e| {
            BamlRtError::InvalidArgument(format!("invalid content_hash in deployment row: {e}"))
        })?,
        agent_name: parsed.agent_name,
        deployed_at: parsed.deployed_at,
        status: parsed.status,
        last_error: parsed.last_error,
        last_attempt_at: parsed.last_attempt_at,
        failure_count: parsed.failure_count,
    })
}

fn to_write_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(format!(
        "runner state write failed: {err}"
    )))
}

fn to_read_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(format!(
        "runner state read failed: {err}"
    )))
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

    #[tokio::test]
    async fn save_and_remove_roundtrip() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let record = DeploymentRecord {
            content_hash: "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            agent_name: "clickup-agent".to_string(),
            deployed_at: "2026-03-25T16:30:00Z".to_string(),
            status: DeploymentStatus::Active,
            last_error: None,
            last_attempt_at: Some("2026-03-25T16:30:00Z".to_string()),
            failure_count: 0,
        };

        store.save_deployment(&record).await.unwrap();
        let records = store.list_deployments().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], record);

        let removed = store
            .remove_deployment(
                &"1111111111111111111111111111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(removed);
        let removed_again = store
            .remove_deployment(
                &"1111111111111111111111111111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!removed_again);
    }

    #[tokio::test]
    async fn persists_across_reopen_on_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        let record = DeploymentRecord {
            content_hash: "2222222222222222222222222222222222222222222222222222222222222222"
                .parse()
                .unwrap(),
            agent_name: "persist-agent".to_string(),
            deployed_at: "2026-03-25T17:00:00Z".to_string(),
            status: DeploymentStatus::Failed,
            last_error: Some("boot failed".to_string()),
            last_attempt_at: Some("2026-03-25T17:00:00Z".to_string()),
            failure_count: 1,
        };
        store.save_deployment(&record).await.unwrap();
        drop(store);

        let reopened = {
            let mut reopened = None;
            for _ in 0..20 {
                match DeploymentStateStore::open(&path).await {
                    Ok(store) => {
                        reopened = Some(store);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
                }
            }
            reopened.expect("reopen deployment state store after lock release")
        };
        let mut records = Vec::new();
        for _ in 0..50 {
            records = reopened.list_deployments().await.unwrap();
            if !records.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].content_hash.as_str(),
            "2222222222222222222222222222222222222222222222222222222222222222"
        );
        assert_eq!(records[0].agent_name, "persist-agent");
        assert_eq!(records[0].status, DeploymentStatus::Failed);
        assert_eq!(records[0].failure_count, 1);
    }
}
