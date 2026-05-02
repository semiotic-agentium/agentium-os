//! Config store backed by SurrealDB.
//!
//! Keyed by bundle name. Stores current config, version history, and
//! internal key-value state (e.g. secret link mappings).

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_tools::{BundleName, ConfigResolver};
use serde_json::Value;
use surrealdb::{Surreal, engine::any::Any};

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
    UnixMs(baml_rt_core::now_unix_ms("config_store"))
}

pub struct SurrealConfigStore {
    db: Arc<Surreal<Any>>,
}

impl SurrealConfigStore {
    /// File-backed SurrealKV store at the given directory path.
    pub async fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let endpoint = format!("surrealkv://{}", path.as_ref().to_string_lossy());
        let db = surrealdb::engine::any::connect(&endpoint)
            .await
            .map_err(map_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(map_err)?;
        init_schema(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Isolated in-memory store. Each call selects a UUID-scoped namespace
    /// so callers get independent keyspaces (sufficient for test isolation).
    pub async fn in_memory() -> Result<Self> {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .map_err(map_err)?;
        let scope = format!("cfg_{}", uuid::Uuid::new_v4().simple());
        db.use_ns(&scope).use_db(&scope).await.map_err(map_err)?;
        init_schema(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Remote SurrealDB over WebSocket. Uses the fixed namespace `config` /
    /// database `store` so all runners sharing the same endpoint see the same
    /// config state.
    pub async fn remote(endpoint: &str, credentials: Option<(&str, &str)>) -> Result<Self> {
        let db = surrealdb::engine::any::connect(endpoint)
            .await
            .map_err(map_err)?;
        if let Some((username, password)) = credentials {
            db.signin(surrealdb::opt::auth::Root {
                username: username.to_string(),
                password: password.to_string(),
            })
            .await
            .map_err(map_err)?;
        }
        db.use_ns(NS).use_db(DB_NAME).await.map_err(map_err)?;
        init_schema(&db).await?;
        Ok(Self { db: Arc::new(db) })
    }
}

async fn init_schema(db: &Surreal<Any>) -> Result<()> {
    init_schema_with(db, SCHEMA_QUERIES).await
}

async fn init_schema_with(db: &Surreal<Any>, queries: &[&str]) -> Result<()> {
    let batch = queries.join("; ");
    db.query(batch)
        .await
        .map_err(map_err)?
        .check()
        .map_err(map_err)?;
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
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
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
        self.db
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
            .map_err(map_err)?
            .check()
            .map_err(map_err)?;

        // After COMMIT, read back the version we just wrote
        let mut ver_resp = self
            .db
            .query("SELECT version OMIT id FROM config_current WHERE bundle_name = $name LIMIT 1")
            .bind(("name", name))
            .await
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
            .map_err(map_err)?;
        self.db
            .query("DELETE FROM config_version_history WHERE bundle_name = $name")
            .bind(("name", name))
            .await
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
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
            .map_err(map_err)?
            .check()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `.check()`, runtime errors inside a multi-statement batch are
    /// silently swallowed by the SurrealDB SDK — the outer `.await` only surfaces
    /// parse / transport / authentication failures, not per-statement runtime
    /// errors. This test pins that contract: a batch whose final statement throws
    /// at execution time must surface as `Err` once `.check()` is chained.
    #[tokio::test]
    async fn check_surfaces_inner_statement_error_in_batch() {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("connect mem");
        db.use_ns("t").use_db("t").await.expect("use ns/db");

        // First statement is valid; the second triggers a runtime error.
        // Without `.check()`, the SDK reports overall success — the silent-swallow
        // behavior this issue is fixing.
        let batch = "DEFINE TABLE IF NOT EXISTS ok SCHEMAFULL; THROW 'inner statement failed';";
        let response = db
            .query(batch)
            .await
            .expect("await must succeed; the runtime error lives inside the response");
        assert!(
            response.check().is_err(),
            "an inner runtime error must be surfaced via Response::check"
        );
    }

    /// Regression test for issue #291: a malformed schema statement injected into
    /// the init batch must propagate as an `Err` from the public store
    /// initialization path, instead of silently completing with a broken schema.
    #[tokio::test]
    async fn init_schema_with_invalid_statement_returns_err() {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("connect mem");
        db.use_ns("t").use_db("t").await.expect("use ns/db");

        let valid_then_failing = &[
            "DEFINE TABLE IF NOT EXISTS ok SCHEMAFULL",
            "THROW 'schema init regression #291'",
        ];

        let result = init_schema_with(&db, valid_then_failing).await;
        assert!(
            result.is_err(),
            "init_schema_with must fail when any statement in the batch errors at runtime"
        );
    }
}
