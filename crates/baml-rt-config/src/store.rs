//! Config store using GraphQLite's SQLite connection: standard tables with JSON in columns.
//!
//! Config is keyed by bundle name. Tables: config_current (current snapshot),
//! config_version_history (version log). No Cypher; raw SQL only.

use std::{path::Path, sync::Mutex};

use baml_rt_core::Result;
use baml_rt_tools::{BundleName, ConfigResolver};
use graphqlite::Connection;
use serde_json::Value;

use crate::{
    error::ConfigStoreError,
    traits::{
        ConfigReader, ConfigService, ConfigVersion, ConfigVersionNumber, ConfigWriter,
        InternalConfigReader, InternalConfigWriter, UnixMs,
    },
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS config_current (
    bundle_name TEXT PRIMARY KEY NOT NULL,
    config_json TEXT NOT NULL,
    version INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS config_version_history (
    bundle_name TEXT NOT NULL,
    version INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (bundle_name, version)
);
CREATE TABLE IF NOT EXISTS config_internal (
    key TEXT PRIMARY KEY NOT NULL,
    config_json TEXT NOT NULL
);
";

fn is_no_rows(e: &impl std::fmt::Display) -> bool {
    let s = e.to_string();
    s.contains("query returned no rows") || s.contains("Query returned no rows")
}

/// SQLite-backed config store (GraphQLite connection, standard tables, JSON in TEXT columns).
pub struct SqliteConfigStore {
    conn: Mutex<Connection>,
}

impl SqliteConfigStore {
    /// Open or create the config database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        conn.sqlite_connection()
            .execute_batch(SCHEMA)
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory store for testing.
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        conn.sqlite_connection()
            .execute_batch(SCHEMA)
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn now_ms() -> UnixMs {
        UnixMs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        )
    }
}

impl ConfigReader for SqliteConfigStore {
    fn get(&self, bundle_name: &BundleName) -> Result<Option<Value>> {
        self.get_with_version(bundle_name)
            .map(|opt| opt.map(|s| s.config))
    }

    fn get_with_version(&self, bundle_name: &BundleName) -> Result<Option<crate::StoredConfig>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let name = bundle_name.as_str();
        let sqlite = conn.sqlite_connection();
        let mut stmt = sqlite
            .prepare("SELECT config_json, version FROM config_current WHERE bundle_name = ?1")
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let mut rows = stmt
            .query([name])
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let row = match rows
            .next()
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?
        {
            Some(r) => r,
            None => return Ok(None),
        };
        let config_json: String = row
            .get(0)
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let version: i64 = row
            .get(1)
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let config: Value = serde_json::from_str(&config_json)?;
        Ok(Some(crate::StoredConfig {
            config,
            version: ConfigVersionNumber(version as u64),
        }))
    }

    fn list_with_config(&self) -> Result<Vec<BundleName>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let sqlite = conn.sqlite_connection();
        let mut stmt = sqlite
            .prepare("SELECT bundle_name FROM config_current")
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for name in rows {
            let name = name.map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
            let bundle_name = BundleName::new(name)
                .map_err(|e| ConfigStoreError::Storage(format!("invalid bundle name: {e}")))?;
            out.push(bundle_name);
        }
        Ok(out)
    }
}

impl ConfigWriter for SqliteConfigStore {
    fn set(&self, bundle_name: &BundleName, config: Value) -> Result<ConfigVersion> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let config_json = serde_json::to_string(&config)?;
        let name = bundle_name.as_str();
        let now_ms = Self::now_ms();

        let sqlite = conn.sqlite_connection();
        let current_version: u64 = match sqlite.query_row(
            "SELECT version FROM config_current WHERE bundle_name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(v) => v as u64,
            Err(e) if is_no_rows(&e) => 0,
            Err(e) => return Err(ConfigStoreError::Storage(e.to_string()).into()),
        };
        let new_version = current_version + 1;

        sqlite
            .execute(
                "INSERT INTO config_current (bundle_name, config_json, version, updated_at_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(bundle_name) DO UPDATE SET config_json = excluded.config_json, version = excluded.version, updated_at_ms = excluded.updated_at_ms",
                (name, config_json.as_str(), new_version as i64, now_ms.0 as i64),
            )
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;

        sqlite
            .execute(
                "INSERT INTO config_version_history (bundle_name, version, config_json, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                (name, new_version as i64, config_json.as_str(), now_ms.0 as i64),
            )
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;

        Ok(ConfigVersion {
            bundle_name: bundle_name.clone(),
            version: ConfigVersionNumber(new_version),
            config,
            created_at_ms: now_ms,
        })
    }

    fn delete(&self, bundle_name: &BundleName) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let name = bundle_name.as_str();
        let sqlite = conn.sqlite_connection();
        sqlite
            .execute("DELETE FROM config_current WHERE bundle_name = ?1", [name])
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        sqlite
            .execute(
                "DELETE FROM config_version_history WHERE bundle_name = ?1",
                [name],
            )
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_version(&self, bundle_name: &BundleName, version: u64) -> Result<Option<ConfigVersion>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let name = bundle_name.as_str();
        let sqlite = conn.sqlite_connection();
        let row: Option<(String, i64)> = match sqlite.query_row(
            "SELECT config_json, created_at_ms FROM config_version_history WHERE bundle_name = ?1 AND version = ?2",
            (name, version as i64),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        ) {
            Ok(r) => Some(r),
            Err(e) if is_no_rows(&e) => None,
            Err(e) => return Err(ConfigStoreError::Storage(e.to_string()).into()),
        };
        let (config_json, created_at_ms) = match row {
            Some(r) => r,
            None => return Ok(None),
        };
        let config: Value = serde_json::from_str(&config_json)?;
        Ok(Some(ConfigVersion {
            bundle_name: bundle_name.clone(),
            version: ConfigVersionNumber(version),
            config,
            created_at_ms: UnixMs(created_at_ms as u64),
        }))
    }

    fn list_versions(&self, bundle_name: &BundleName) -> Result<Vec<ConfigVersion>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let name = bundle_name.as_str();
        let sqlite = conn.sqlite_connection();
        let mut stmt = sqlite
            .prepare(
                "SELECT version, config_json, created_at_ms FROM config_version_history WHERE bundle_name = ?1 ORDER BY version DESC",
            )
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([name], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (version, config_json, created_at_ms) =
                row.map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
            let config: Value = serde_json::from_str(&config_json)?;
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

impl InternalConfigReader for SqliteConfigStore {
    fn get_internal(&self, key: &str) -> Result<Option<Value>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let sqlite = conn.sqlite_connection();
        let mut stmt = sqlite
            .prepare("SELECT config_json FROM config_internal WHERE key = ?1")
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let mut rows = stmt
            .query([key])
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let row = match rows
            .next()
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?
        {
            Some(r) => r,
            None => return Ok(None),
        };
        let config_json: String = row
            .get(0)
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        let config: Value = serde_json::from_str(&config_json)?;
        Ok(Some(config))
    }
}

impl InternalConfigWriter for SqliteConfigStore {
    fn set_internal(&self, key: &str, value: Value) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ConfigStoreError::LockPoisoned(e.to_string()))?;
        let config_json = serde_json::to_string(&value)?;
        let sqlite = conn.sqlite_connection();
        sqlite
            .execute(
                "INSERT INTO config_internal (key, config_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET config_json = excluded.config_json",
                (key, config_json.as_str()),
            )
            .map_err(|e| ConfigStoreError::Storage(e.to_string()))?;
        Ok(())
    }
}

impl ConfigService for SqliteConfigStore {}

impl ConfigResolver for SqliteConfigStore {
    fn get_config(&self, bundle_name: &BundleName) -> baml_rt_core::Result<Option<Value>> {
        ConfigReader::get(self, bundle_name)
    }

    fn get_config_with_version(
        &self,
        bundle_name: &BundleName,
    ) -> baml_rt_core::Result<Option<(Value, u64)>> {
        ConfigReader::get_with_version(self, bundle_name)
            .map(|opt| opt.map(|s| (s.config, s.version.into())))
    }
}
