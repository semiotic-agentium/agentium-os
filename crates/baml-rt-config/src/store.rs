//! Config store backed by SurrealDB embedded.
//!
//! Config is keyed by bundle name. Uses SurrealDB tables for current config,
//! version history, and internal key-value config.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_tools::{BundleName, ConfigResolver};
use serde_json::Value;
use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};

use crate::{
    error::ConfigStoreError,
    traits::{
        ConfigReader, ConfigService, ConfigVersion, ConfigVersionNumber, ConfigWriter,
        InternalConfigReader, InternalConfigWriter, UnixMs,
    },
};

const NS: &str = "config";
const DB_NAME: &str = "store";

const SCHEMA_QUERIES: &[&str] = &[
    "DEFINE TABLE IF NOT EXISTS config_current SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS bundle_name ON config_current TYPE string",
    "DEFINE FIELD IF NOT EXISTS config_json ON config_current TYPE string",
    "DEFINE FIELD IF NOT EXISTS version ON config_current TYPE int",
    "DEFINE FIELD IF NOT EXISTS updated_at_ms ON config_current TYPE int",
    "DEFINE INDEX IF NOT EXISTS idx_cc_bundle ON config_current FIELDS bundle_name UNIQUE",
    "DEFINE TABLE IF NOT EXISTS config_version_history SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS bundle_name ON config_version_history TYPE string",
    "DEFINE FIELD IF NOT EXISTS version ON config_version_history TYPE int",
    "DEFINE FIELD IF NOT EXISTS config_json ON config_version_history TYPE string",
    "DEFINE FIELD IF NOT EXISTS created_at_ms ON config_version_history TYPE int",
    "DEFINE INDEX IF NOT EXISTS idx_cvh_bundle_ver ON config_version_history FIELDS bundle_name, version UNIQUE",
    "DEFINE TABLE IF NOT EXISTS config_internal SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS key ON config_internal TYPE string",
    "DEFINE FIELD IF NOT EXISTS config_json ON config_internal TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_ci_key ON config_internal FIELDS key UNIQUE",
];

fn map_err(e: surrealdb::Error) -> ConfigStoreError {
    ConfigStoreError::Storage(e.to_string())
}

fn now_ms() -> UnixMs {
    UnixMs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    )
}

pub struct SurrealConfigStore {
    db: Arc<Surreal<Db>>,
}

impl SurrealConfigStore {
    pub async fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path.as_ref().to_string_lossy().as_ref())
            .await
            .map_err(map_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(map_err)?;
        init_schema(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }

    pub async fn in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await.map_err(map_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(map_err)?;
        init_schema(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }
}

async fn init_schema(db: &Surreal<Db>) -> Result<()> {
    for stmt in SCHEMA_QUERIES {
        db.query(*stmt).await.map_err(map_err)?;
    }
    Ok(())
}

#[async_trait]
impl ConfigReader for SurrealConfigStore {
    async fn get(&self, bundle_name: &BundleName) -> Result<Option<Value>> {
        self.get_with_version(bundle_name)
            .await
            .map(|opt| opt.map(|s| s.config))
    }

    async fn get_with_version(
        &self,
        bundle_name: &BundleName,
    ) -> Result<Option<crate::StoredConfig>> {
        let name = bundle_name.as_str().to_string();
        let mut resp = self
            .db
            .query(
                "SELECT config_json, version FROM config_current WHERE bundle_name = $name LIMIT 1",
            )
            .bind(("name", name))
            .await
            .map_err(map_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                let config_json = row
                    .get("config_json")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let version = row.get("version").and_then(Value::as_i64).unwrap_or(0);
                let config: Value = serde_json::from_str(config_json)?;
                Ok(Some(crate::StoredConfig {
                    config,
                    version: ConfigVersionNumber(version as u64),
                }))
            }
        }
    }

    async fn list_with_config(&self) -> Result<Vec<BundleName>> {
        let mut resp = self
            .db
            .query("SELECT bundle_name FROM config_current")
            .await
            .map_err(map_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
        let mut out = Vec::new();
        for row in &rows {
            if let Some(name) = row.get("bundle_name").and_then(Value::as_str) {
                let bn = BundleName::new(name.to_string())
                    .map_err(|e| ConfigStoreError::Storage(format!("invalid bundle name: {e}")))?;
                out.push(bn);
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl ConfigWriter for SurrealConfigStore {
    async fn set(&self, bundle_name: &BundleName, config: Value) -> Result<ConfigVersion> {
        let name = bundle_name.as_str().to_string();
        let config_json = serde_json::to_string(&config)?;
        let ts = now_ms();

        // Atomic version increment via BEGIN/COMMIT transaction.
        // All three statements execute as one atomic batch — concurrent callers
        // cannot interleave between the SELECT and the writes.
        let _resp = self.db
            .query("\
                BEGIN; \
                LET $cur = (SELECT version FROM config_current WHERE bundle_name = $name LIMIT 1); \
                LET $safe = $cur ?? []; \
                LET $nv = IF array::len($safe) > 0 THEN $safe[0].version + 1 ELSE 1 END; \
                UPSERT config_current SET bundle_name = $name, config_json = $cj, version = $nv, updated_at_ms = $ts WHERE bundle_name = $name; \
                CREATE config_version_history SET bundle_name = $name, version = $nv, config_json = $cj, created_at_ms = $ts; \
                COMMIT;")
            .bind(("name", name.clone()))
            .bind(("cj", config_json))
            .bind(("ts", ts.0 as i64))
            .await
            .map_err(map_err)?;

        // After COMMIT, read back the version we just wrote
        let mut ver_resp = self
            .db
            .query("SELECT version OMIT id FROM config_current WHERE bundle_name = $name LIMIT 1")
            .bind(("name", name))
            .await
            .map_err(map_err)?;
        let ver_rows: Vec<Value> = ver_resp.take(0).map_err(map_err)?;
        let new_version = ver_rows
            .first()
            .and_then(|r| r.get("version").and_then(Value::as_u64))
            .unwrap_or(1);

        Ok(ConfigVersion {
            bundle_name: bundle_name.clone(),
            version: ConfigVersionNumber(new_version),
            config,
            created_at_ms: ts,
        })
    }

    async fn delete(&self, bundle_name: &BundleName) -> Result<()> {
        let name = bundle_name.as_str().to_string();
        self.db
            .query("DELETE FROM config_current WHERE bundle_name = $name")
            .bind(("name", name.clone()))
            .await
            .map_err(map_err)?;
        self.db
            .query("DELETE FROM config_version_history WHERE bundle_name = $name")
            .bind(("name", name))
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn get_version(
        &self,
        bundle_name: &BundleName,
        version: u64,
    ) -> Result<Option<ConfigVersion>> {
        let name = bundle_name.as_str().to_string();
        let mut resp = self.db
            .query("SELECT config_json, created_at_ms FROM config_version_history WHERE bundle_name = $name AND version = $ver LIMIT 1")
            .bind(("name", name))
            .bind(("ver", version as i64))
            .await
            .map_err(map_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                let config_json = row
                    .get("config_json")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let created_at_ms = row
                    .get("created_at_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let config: Value = serde_json::from_str(config_json)?;
                Ok(Some(ConfigVersion {
                    bundle_name: bundle_name.clone(),
                    version: ConfigVersionNumber(version),
                    config,
                    created_at_ms: UnixMs(created_at_ms as u64),
                }))
            }
        }
    }

    async fn list_versions(&self, bundle_name: &BundleName) -> Result<Vec<ConfigVersion>> {
        let name = bundle_name.as_str().to_string();
        let mut resp = self.db
            .query("SELECT version, config_json, created_at_ms FROM config_version_history WHERE bundle_name = $name ORDER BY version DESC")
            .bind(("name", name))
            .await
            .map_err(map_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
        let mut out = Vec::new();
        for row in &rows {
            let version = row.get("version").and_then(Value::as_i64).unwrap_or(0);
            let config_json = row
                .get("config_json")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let created_at_ms = row
                .get("created_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let config: Value = serde_json::from_str(config_json)?;
            out.push(ConfigVersion {
                bundle_name: bundle_name.clone(),
                version: ConfigVersionNumber(version as u64),
                config,
                created_at_ms: UnixMs(created_at_ms as u64),
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl InternalConfigReader for SurrealConfigStore {
    async fn get_internal(&self, key: &str) -> Result<Option<Value>> {
        let key = key.to_string();
        let mut resp = self
            .db
            .query("SELECT config_json FROM config_internal WHERE key = $key LIMIT 1")
            .bind(("key", key))
            .await
            .map_err(map_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
        match rows.first() {
            None => Ok(None),
            Some(row) => {
                let config_json = row
                    .get("config_json")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let config: Value = serde_json::from_str(config_json)?;
                Ok(Some(config))
            }
        }
    }
}

#[async_trait]
impl InternalConfigWriter for SurrealConfigStore {
    async fn set_internal(&self, key: &str, value: Value) -> Result<()> {
        let key = key.to_string();
        let config_json = serde_json::to_string(&value)?;
        self.db
            .query("UPSERT config_internal SET key = $key, config_json = $cj WHERE key = $key")
            .bind(("key", key))
            .bind(("cj", config_json))
            .await
            .map_err(map_err)?;
        Ok(())
    }
}

impl ConfigService for SurrealConfigStore {}

#[async_trait]
impl ConfigResolver for SurrealConfigStore {
    async fn get_config(&self, bundle_name: &BundleName) -> baml_rt_core::Result<Option<Value>> {
        ConfigReader::get(self, bundle_name).await
    }

    async fn get_config_with_version(
        &self,
        bundle_name: &BundleName,
    ) -> baml_rt_core::Result<Option<(Value, u64)>> {
        ConfigReader::get_with_version(self, bundle_name)
            .await
            .map(|opt| opt.map(|s| (s.config, s.version.into())))
    }
}
