// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! SQLite-backed mapping of Grafana alert identity to Agentium `context_id`.
//!
//! Identity reuse semantics (see demo_plan §Provenance Link):
//! - First `firing` for an inactive/new fingerprint creates a new `context_id`.
//! - Repeated `firing` while active reuse the existing `context_id`.
//! - `resolved` reuses the active `context_id`, marks the row resolved.
//! - First `firing` after resolved mints a new `context_id`.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MappingError {
    #[error("mapping store sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("mapping store mutex poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertStatus {
    Firing,
    Resolved,
}

impl AlertStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertStatus::Firing => "firing",
            AlertStatus::Resolved => "resolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertIdentity {
    pub fingerprint: String,
    pub group_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextResolution {
    pub context_id: String,
    pub reused: bool,
}

/// Mint a fresh `context_id`-shaped string. The runner mints real `ContextId`
/// values from temporal sequence; the tool crate must not depend on runner
/// internals, so we use a deterministic-ish suffix derived from `now`.
fn mint_context_id(now_ms: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(now_ms.to_be_bytes());
    let bytes = hasher.finalize();
    let counter = u64::from_be_bytes(bytes[..8].try_into().expect("8 bytes"));
    format!("ctx-{now_ms}-{counter:016x}")
}

#[derive(Clone)]
pub struct MappingStore {
    conn: Arc<Mutex<Connection>>,
}

impl MappingStore {
    pub fn open(path: &Path) -> Result<Self, MappingError> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self, MappingError> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), MappingError> {
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS grafana_alert_context (
                fingerprint    TEXT NOT NULL,
                group_key      TEXT NOT NULL,
                context_id     TEXT NOT NULL,
                status         TEXT NOT NULL,
                active_since   INTEGER NOT NULL,
                resolved_at    INTEGER,
                updated_at     INTEGER NOT NULL,
                PRIMARY KEY (fingerprint, active_since)
            );
            CREATE INDEX IF NOT EXISTS idx_grafana_alert_active
                ON grafana_alert_context(fingerprint, status);
            ",
        )?;
        Ok(())
    }

    /// Resolve (or mint) the `context_id` for a firing/resolved Grafana alert.
    pub fn resolve(
        &self,
        identity: &AlertIdentity,
        status: AlertStatus,
        now_ms: u64,
    ) -> Result<ContextResolution, MappingError> {
        let mut conn = self.conn.lock().map_err(|_| MappingError::Poisoned)?;
        let tx = conn.transaction()?;

        let active: Option<(String, i64)> = tx
            .query_row(
                "SELECT context_id, active_since FROM grafana_alert_context
                 WHERE fingerprint = ?1 AND status = 'firing'
                 ORDER BY active_since DESC LIMIT 1",
                params![identity.fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let resolution = match (status, active) {
            (AlertStatus::Firing, Some((ctx, _))) => ContextResolution {
                context_id: ctx,
                reused: true,
            },
            (AlertStatus::Resolved, Some((ctx, since))) => {
                tx.execute(
                    "UPDATE grafana_alert_context
                     SET status = 'resolved', resolved_at = ?1, updated_at = ?1
                     WHERE fingerprint = ?2 AND active_since = ?3",
                    params![now_ms as i64, identity.fingerprint, since],
                )?;
                ContextResolution {
                    context_id: ctx,
                    reused: true,
                }
            }
            (AlertStatus::Firing, None) => {
                let ctx = mint_context_id(now_ms);
                tx.execute(
                    "INSERT INTO grafana_alert_context
                     (fingerprint, group_key, context_id, status, active_since, resolved_at, updated_at)
                     VALUES (?1, ?2, ?3, 'firing', ?4, NULL, ?4)",
                    params![
                        identity.fingerprint,
                        identity.group_key,
                        ctx,
                        now_ms as i64
                    ],
                )?;
                ContextResolution {
                    context_id: ctx,
                    reused: false,
                }
            }
            (AlertStatus::Resolved, None) => {
                // Resolved without a prior firing — record a synthetic
                // resolved row so observers can audit it, but do not mint a
                // reusable context. Downstream callers should treat reused=false
                // here as a degenerate case.
                let ctx = mint_context_id(now_ms);
                tx.execute(
                    "INSERT INTO grafana_alert_context
                     (fingerprint, group_key, context_id, status, active_since, resolved_at, updated_at)
                     VALUES (?1, ?2, ?3, 'resolved', ?4, ?4, ?4)",
                    params![
                        identity.fingerprint,
                        identity.group_key,
                        ctx,
                        now_ms as i64
                    ],
                )?;
                ContextResolution {
                    context_id: ctx,
                    reused: false,
                }
            }
        };

        tx.commit()?;
        Ok(resolution)
    }

    #[cfg(test)]
    pub fn active_count(&self) -> Result<usize, MappingError> {
        let conn = self.conn.lock().map_err(|_| MappingError::Poisoned)?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM grafana_alert_context WHERE status = 'firing'",
            [],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(fp: &str) -> AlertIdentity {
        AlertIdentity {
            fingerprint: fp.to_string(),
            group_key: format!("group:{fp}"),
        }
    }

    #[test]
    fn first_firing_mints_new_context() {
        let store = MappingStore::open_in_memory().unwrap();
        let r = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 1_000)
            .unwrap();
        assert!(!r.reused);
        assert!(r.context_id.starts_with("ctx-"));
    }

    #[test]
    fn repeated_firing_reuses_context() {
        let store = MappingStore::open_in_memory().unwrap();
        let r1 = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 1_000)
            .unwrap();
        let r2 = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 2_000)
            .unwrap();
        assert_eq!(r1.context_id, r2.context_id);
        assert!(r2.reused);
    }

    #[test]
    fn resolved_reuses_active_then_new_firing_mints_fresh() {
        let store = MappingStore::open_in_memory().unwrap();
        let r1 = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 1_000)
            .unwrap();
        let r2 = store
            .resolve(&identity("fp1"), AlertStatus::Resolved, 2_000)
            .unwrap();
        assert_eq!(r1.context_id, r2.context_id);
        assert!(r2.reused);
        assert_eq!(store.active_count().unwrap(), 0);

        let r3 = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 3_000)
            .unwrap();
        assert_ne!(r3.context_id, r1.context_id);
        assert!(!r3.reused);
    }

    #[test]
    fn distinct_fingerprints_get_distinct_contexts() {
        let store = MappingStore::open_in_memory().unwrap();
        let a = store
            .resolve(&identity("fp1"), AlertStatus::Firing, 1_000)
            .unwrap();
        let b = store
            .resolve(&identity("fp2"), AlertStatus::Firing, 1_001)
            .unwrap();
        assert_ne!(a.context_id, b.context_id);
    }
}
