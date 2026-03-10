//! SQLite-backed metadata, lineage, and search store.
//!
//! A single SQLite database holds all structured data for the repository.
//! One `SqliteStore` instance owns a connection pool (tokio spawn_blocking for
//! each query since rusqlite is synchronous). FTS5 is used for full-text search.
//!
//! ## Schema
//!
//! - `entries` — one row per published agent version (immutable after insert)
//! - `fitness_scores` — append-only evaluation scores
//! - `tags` — many-to-many labels
//! - `lineage_edges` — directed edges in the DAG
//! - `entries_fts` — FTS5 virtual table for full-text search

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use crate::{
    entry::{
        ChangeRationale, FitnessDomain, FitnessScore, NewEntry, RepositoryEntry,
        RepositoryEntryHeader, Tag, Timestamp,
    },
    error::{RepositoryError, Result},
    ids::{AgentName, ContentHash, Generation, LineageEdgeId, Version, VersionRef},
    lineage::{
        AncestryNode, EdgeDescription, LineageEdge, LineageKind, LineageSubgraph, Parentage,
    },
    search::{SearchOrder, SearchQuery},
    storage::{LineageStore, MetadataStore, SearchStore},
};

/// SQLite-backed store for all structured repository data.
///
/// Thread-safe via `Mutex<Connection>`. All database operations are dispatched
/// to `spawn_blocking` to avoid blocking the async runtime.
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// Open or create a SQLite database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        // Schema is created synchronously at open time (not async).
        // This is fine for init — we're not inside a tokio context yet necessarily.
        // We'll use the blocking approach.
        Ok(store)
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        Ok(store)
    }

    /// Initialize the schema. Must be called once after opening.
    pub async fn init_schema(&self) -> Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute_batch(SCHEMA_SQL)
                .map_err(|e| RepositoryError::StorageWrite {
                    source: Box::new(e),
                })
        })
        .await
        .map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?
    }

    /// Run a blocking closure with the connection.
    async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            f(&conn)
        })
        .await
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?
    }
}

// ---------------------------------------------------------------------------
// Schema DDL
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    hash TEXT PRIMARY KEY,
    agent_name TEXT NOT NULL,
    version INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    parentage_json TEXT NOT NULL,
    source_json TEXT NOT NULL,
    change_rationale TEXT NOT NULL,
    created_at TEXT NOT NULL,
    manifest_description TEXT,
    manifest_tools_json TEXT NOT NULL DEFAULT '[]',
    manifest_capabilities_json TEXT NOT NULL DEFAULT '[]',
    UNIQUE(agent_name, version)
);

CREATE INDEX IF NOT EXISTS idx_entries_name ON entries(agent_name);
CREATE INDEX IF NOT EXISTS idx_entries_name_version ON entries(agent_name, version DESC);

CREATE TABLE IF NOT EXISTS fitness_scores (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_hash TEXT NOT NULL REFERENCES entries(hash),
    domain TEXT NOT NULL,
    score REAL NOT NULL,
    recorded_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_fitness_hash ON fitness_scores(entry_hash);
CREATE INDEX IF NOT EXISTS idx_fitness_domain_score ON fitness_scores(domain, score DESC);

CREATE TABLE IF NOT EXISTS tags (
    entry_hash TEXT NOT NULL REFERENCES entries(hash),
    tag TEXT NOT NULL,
    PRIMARY KEY(entry_hash, tag)
);

CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);

CREATE TABLE IF NOT EXISTS lineage_edges (
    id TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL,
    target_hash TEXT NOT NULL REFERENCES entries(hash),
    kind TEXT NOT NULL CHECK(kind IN ('fork', 'influence')),
    description TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_lineage_source ON lineage_edges(source_hash);
CREATE INDEX IF NOT EXISTS idx_lineage_target ON lineage_edges(target_hash);

CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    hash,
    agent_name,
    source_text,
    manifest_text,
    content='',
    tokenize='porter unicode61'
);
"#;

// ---------------------------------------------------------------------------
// MetadataStore implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl MetadataStore for SqliteStore {
    async fn insert_entry(&self, entry: &NewEntry) -> Result<RepositoryEntry> {
        let name = entry.name.as_str().to_string();
        let generation = entry.generation.as_u32();
        let parentage_json =
            serde_json::to_string(&entry.parentage).expect("parentage serialization");
        let rationale = entry.change_rationale.as_str().to_string();

        // Collect FTS text (does not depend on version)
        let mut source_text = String::new();
        for f in &entry.source.ts_sources {
            source_text.push_str(f.content.as_str());
            source_text.push('\n');
        }
        for f in &entry.source.baml_sources {
            source_text.push_str(f.content.as_str());
            source_text.push('\n');
        }

        // Tags to insert
        let tags: Vec<String> = entry.tags.iter().map(|t| t.as_str().to_string()).collect();

        // Clone source for the blocking closure (version will be set inside)
        let source_bundle = entry.source.clone();

        // Compute timestamp now (before entering the blocking closure)
        let now = crate::service::chrono_now();
        let created_at = now.as_str().to_string();

        self.with_conn(move |conn| {
            // Atomically determine next version inside the same connection lock.
            // No TOCTOU race: we hold the Mutex<Connection> for the entire operation.
            let max: Option<u32> = conn
                .query_row(
                    "SELECT MAX(version) FROM entries WHERE agent_name = ?1",
                    params![name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .flatten();
            let version = match max {
                Some(v) => v + 1,
                None => 1,
            };

            // Write the repository-assigned version into the manifest, then
            // compute the canonical hash. The hash covers the versioned manifest.
            let versioned_source = source_bundle.with_manifest_version(version);
            let hash = versioned_source.compute_hash();
            let hash_str = hash.as_str().to_string();

            let source_json =
                serde_json::to_string(&versioned_source).expect("source serialization");
            let description = versioned_source.manifest.description().map(String::from);
            let tools_json = serde_json::to_string(
                &versioned_source
                    .manifest
                    .tools()
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            let caps_json = serde_json::to_string(
                &versioned_source
                    .manifest
                    .capabilities()
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            let manifest_text =
                serde_json::to_string(versioned_source.manifest.as_value()).unwrap_or_default();

            conn.execute(
                "INSERT INTO entries (hash, agent_name, version, generation, parentage_json,
                 source_json, change_rationale, created_at, manifest_description,
                 manifest_tools_json, manifest_capabilities_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    hash_str,
                    name,
                    version,
                    generation,
                    parentage_json,
                    source_json,
                    rationale,
                    created_at,
                    description,
                    tools_json,
                    caps_json,
                ],
            )
            .map_err(|e| {
                if let rusqlite::Error::SqliteFailure(ref err, _) = e
                    && err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                {
                    return RepositoryError::DuplicateHash {
                        hash: hash_str.parse().unwrap(),
                        existing_name: name.parse().unwrap(),
                        existing_version: Version::new(version).unwrap(),
                    };
                }
                RepositoryError::StorageWrite {
                    source: Box::new(e),
                }
            })?;

            // FTS index
            conn.execute(
                "INSERT INTO entries_fts (hash, agent_name, source_text, manifest_text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash_str, name, source_text, manifest_text],
            )
            .map_err(|e| RepositoryError::StorageWrite {
                source: Box::new(e),
            })?;

            // Tags
            for tag in &tags {
                conn.execute(
                    "INSERT OR IGNORE INTO tags (entry_hash, tag) VALUES (?1, ?2)",
                    params![hash_str, tag],
                )
                .map_err(|e| RepositoryError::StorageWrite {
                    source: Box::new(e),
                })?;
            }

            // Build the complete RepositoryEntry with the assigned version and hash.
            let assigned_version = Version::new(version).expect("version > 0");
            let parentage: Parentage =
                serde_json::from_str(&parentage_json).expect("parentage round-trip");
            let change_rationale = ChangeRationale::new(rationale).expect("rationale round-trip");
            let tags_out = tags.iter().map(|t| Tag::new(t.as_str())).collect();

            Ok(RepositoryEntry {
                hash,
                version_ref: VersionRef {
                    name: name.parse().unwrap(),
                    version: assigned_version,
                },
                source: versioned_source,
                parentage,
                generation: Generation::new(generation),
                change_rationale,
                created_at: Timestamp::new(created_at),
                fitness_scores: vec![],
                tags: tags_out,
            })
        })
        .await
    }

    async fn get_by_hash(&self, hash: &ContentHash) -> Result<Option<RepositoryEntry>> {
        let hash_str = hash.as_str().to_string();
        self.with_conn(move |conn| load_entry(conn, "hash = ?1", &[&hash_str]))
            .await
    }

    async fn get_by_version(
        &self,
        name: &AgentName,
        version: Version,
    ) -> Result<Option<RepositoryEntry>> {
        let name_str = name.as_str().to_string();
        let ver = version.as_u32();
        self.with_conn(move |conn| {
            load_entry(
                conn,
                "agent_name = ?1 AND version = ?2",
                &[&name_str as &dyn rusqlite::types::ToSql, &ver],
            )
        })
        .await
    }

    async fn get_latest(&self, name: &AgentName) -> Result<Option<RepositoryEntry>> {
        let name_str = name.as_str().to_string();
        self.with_conn(move |conn| {
            load_entry(
                conn,
                "agent_name = ?1 ORDER BY version DESC LIMIT 1",
                &[&name_str],
            )
        })
        .await
    }

    async fn resolve_hash(&self, version_ref: &VersionRef) -> Result<Option<ContentHash>> {
        let name_str = version_ref.name.as_str().to_string();
        let ver = version_ref.version.as_u32();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT hash FROM entries WHERE agent_name = ?1 AND version = ?2")
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let result = stmt
                .query_row(params![name_str, ver], |row| {
                    let h: String = row.get(0)?;
                    Ok(h)
                })
                .optional()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            match result {
                Some(h) => Ok(Some(h.parse().map_err(|e| {
                    RepositoryError::StorageRead {
                        source: Box::new(e),
                    }
                })?)),
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_versions(&self, name: &AgentName) -> Result<Vec<RepositoryEntryHeader>> {
        let name_str = name.as_str().to_string();
        self.with_conn(move |conn| {
            load_headers(
                conn,
                "WHERE agent_name = ?1 ORDER BY version DESC",
                &[&name_str],
            )
        })
        .await
    }

    async fn list_agents(&self) -> Result<Vec<AgentName>> {
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT DISTINCT agent_name FROM entries ORDER BY agent_name")
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let names = stmt
                .query_map([], |row| {
                    let name: String = row.get(0)?;
                    Ok(name)
                })
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            names
                .into_iter()
                .map(|n| {
                    n.parse().map_err(|e| RepositoryError::StorageRead {
                        source: Box::new(e),
                    })
                })
                .collect()
        })
        .await
    }

    async fn record_fitness(
        &self,
        hash: &ContentHash,
        domain: FitnessDomain,
        score: f64,
        recorded_at: Timestamp,
    ) -> Result<()> {
        let hash_str = hash.as_str().to_string();
        let domain_str = domain.as_str().to_string();
        let at_str = recorded_at.as_str().to_string();
        self.with_conn(move |conn| {
            // Verify entry exists
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM entries WHERE hash = ?1",
                    params![hash_str],
                    |_| Ok(true),
                )
                .optional()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .unwrap_or(false);

            if !exists {
                return Err(RepositoryError::EntryNotFoundByHash {
                    hash: hash_str.parse().unwrap(),
                });
            }

            conn.execute(
                "INSERT INTO fitness_scores (entry_hash, domain, score, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash_str, domain_str, score, at_str],
            )
            .map_err(|e| RepositoryError::StorageWrite {
                source: Box::new(e),
            })?;
            Ok(())
        })
        .await
    }

    async fn add_tag(&self, hash: &ContentHash, tag: Tag) -> Result<()> {
        let hash_str = hash.as_str().to_string();
        let tag_str = tag.as_str().to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO tags (entry_hash, tag) VALUES (?1, ?2)",
                params![hash_str, tag_str],
            )
            .map_err(|e| RepositoryError::StorageWrite {
                source: Box::new(e),
            })?;
            Ok(())
        })
        .await
    }

    async fn remove_tag(&self, hash: &ContentHash, tag: &Tag) -> Result<()> {
        let hash_str = hash.as_str().to_string();
        let tag_str = tag.as_str().to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM tags WHERE entry_hash = ?1 AND tag = ?2",
                params![hash_str, tag_str],
            )
            .map_err(|e| RepositoryError::StorageWrite {
                source: Box::new(e),
            })?;
            Ok(())
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// LineageStore implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LineageStore for SqliteStore {
    async fn record_edges(&self, edges: &[LineageEdge]) -> Result<()> {
        let edges: Vec<(String, String, String, String, String)> = edges
            .iter()
            .map(|e| {
                (
                    e.id.as_str().to_string(),
                    e.source.as_str().to_string(),
                    e.target.as_str().to_string(),
                    match e.kind {
                        LineageKind::Fork => "fork".to_string(),
                        LineageKind::Influence => "influence".to_string(),
                    },
                    e.description.as_str().to_string(),
                )
            })
            .collect();

        self.with_conn(move |conn| {
            for (id, source, target, kind, desc) in &edges {
                conn.execute(
                    "INSERT INTO lineage_edges (id, source_hash, target_hash, kind, description)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, source, target, kind, desc],
                )
                .map_err(|e| RepositoryError::StorageWrite {
                    source: Box::new(e),
                })?;
            }
            Ok(())
        })
        .await
    }

    async fn parents(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let hash_str = hash.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT e.hash, e.generation, e.parentage_json
                     FROM lineage_edges le
                     JOIN entries e ON e.hash = le.source_hash
                     WHERE le.target_hash = ?1",
                )
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let nodes = stmt
                .query_map(params![hash_str], |row| Ok(row_to_ancestry_node(row)))
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            Ok(nodes)
        })
        .await
    }

    async fn children(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let hash_str = hash.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT e.hash, e.generation, e.parentage_json
                     FROM lineage_edges le
                     JOIN entries e ON e.hash = le.target_hash
                     WHERE le.source_hash = ?1",
                )
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let nodes = stmt
                .query_map(params![hash_str], |row| Ok(row_to_ancestry_node(row)))
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            Ok(nodes)
        })
        .await
    }

    async fn ancestors(&self, hash: &ContentHash, max_depth: u32) -> Result<Vec<AncestryNode>> {
        let hash_str = hash.as_str().to_string();
        self.with_conn(move |conn| {
            // Recursive CTE to walk ancestors
            let sql = format!(
                "WITH RECURSIVE anc(hash, depth) AS (
                    SELECT source_hash, 1 FROM lineage_edges WHERE target_hash = ?1
                    UNION
                    SELECT le.source_hash, anc.depth + 1
                    FROM lineage_edges le
                    JOIN anc ON anc.hash = le.target_hash
                    WHERE anc.depth < {max_depth}
                )
                SELECT DISTINCT e.hash, e.generation, e.parentage_json
                FROM anc
                JOIN entries e ON e.hash = anc.hash
                ORDER BY e.generation ASC"
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let nodes = stmt
                .query_map(params![hash_str], |row| Ok(row_to_ancestry_node(row)))
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            Ok(nodes)
        })
        .await
    }

    async fn influenced_by(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let hash_str = hash.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT e.hash, e.generation, e.parentage_json
                     FROM lineage_edges le
                     JOIN entries e ON e.hash = le.target_hash
                     WHERE le.source_hash = ?1 AND le.kind = 'influence'",
                )
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            let nodes = stmt
                .query_map(params![hash_str], |row| Ok(row_to_ancestry_node(row)))
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::StorageRead {
                    source: Box::new(e),
                })?;
            Ok(nodes)
        })
        .await
    }

    async fn subgraph(&self, hash: &ContentHash, ancestor_depth: u32) -> Result<LineageSubgraph> {
        let ancestors = self.ancestors(hash, ancestor_depth).await?;
        let descendants = self.children(hash).await?;

        // Collect all hashes in the subgraph
        let mut all_hashes: Vec<String> = ancestors
            .iter()
            .map(|n| n.hash.as_str().to_string())
            .collect();
        all_hashes.push(hash.as_str().to_string());
        for d in &descendants {
            all_hashes.push(d.hash.as_str().to_string());
        }

        // Load edges within the subgraph
        let edges = self
            .with_conn(move |conn| {
                let placeholders: String =
                    all_hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT id, source_hash, target_hash, kind, description
                 FROM lineage_edges
                 WHERE source_hash IN ({placeholders}) OR target_hash IN ({placeholders})"
                );
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| RepositoryError::StorageRead {
                        source: Box::new(e),
                    })?;

                // Bind params: each hash appears twice (source IN + target IN)
                let mut param_values: Vec<String> = Vec::new();
                param_values.extend(all_hashes.iter().cloned());
                param_values.extend(all_hashes.iter().cloned());
                let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
                    .iter()
                    .map(|s| s as &dyn rusqlite::types::ToSql)
                    .collect();

                let edges = stmt
                    .query_map(param_refs.as_slice(), |row| {
                        let id: String = row.get(0)?;
                        let source: String = row.get(1)?;
                        let target: String = row.get(2)?;
                        let kind: String = row.get(3)?;
                        let desc: String = row.get(4)?;
                        Ok((id, source, target, kind, desc))
                    })
                    .map_err(|e| RepositoryError::StorageRead {
                        source: Box::new(e),
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| RepositoryError::StorageRead {
                        source: Box::new(e),
                    })?;

                let mut result = Vec::new();
                for (id, source, target, kind, desc) in edges {
                    result.push(LineageEdge {
                        id: LineageEdgeId::from_uuid(
                            uuid::Uuid::parse_str(&id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
                        ),
                        source: source.parse().unwrap(),
                        target: target.parse().unwrap(),
                        kind: match kind.as_str() {
                            "fork" => LineageKind::Fork,
                            _ => LineageKind::Influence,
                        },
                        description: EdgeDescription::new(desc)
                            .unwrap_or_else(|_| EdgeDescription::new("(no description)").unwrap()),
                    });
                }
                Ok(result)
            })
            .await?;

        Ok(LineageSubgraph {
            root: hash.clone(),
            ancestors,
            descendants,
            edges,
        })
    }
}

// ---------------------------------------------------------------------------
// SearchStore implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl SearchStore for SqliteStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>> {
        let query = query.clone();
        self.with_conn(move |conn| {
            let mut conditions: Vec<String> = Vec::new();
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql + Send>> = Vec::new();
            let mut param_idx = 1u32;

            // Full-text search
            let mut fts_join = String::new();
            if let Some(ref text) = query.text {
                fts_join = format!(
                    " JOIN entries_fts fts ON fts.hash = e.hash AND entries_fts MATCH ?{param_idx}"
                );
                param_values.push(Box::new(text.as_str().to_string()));
                param_idx += 1;
            }

            // Agent name filter
            if let Some(ref name) = query.name {
                conditions.push(format!("e.agent_name = ?{param_idx}"));
                param_values.push(Box::new(name.as_str().to_string()));
                param_idx += 1;
            }

            // Tag filters (all must match)
            for tag in &query.tags {
                conditions.push(format!(
                    "EXISTS (SELECT 1 FROM tags t WHERE t.entry_hash = e.hash AND t.tag = ?{param_idx})"
                ));
                param_values.push(Box::new(tag.as_str().to_string()));
                param_idx += 1;
            }

            // Tool filters
            for tool in &query.tools {
                conditions.push(format!(
                    "e.manifest_tools_json LIKE ?{param_idx}"
                ));
                param_values.push(Box::new(format!("%\"{}\"%", tool.as_str())));
                param_idx += 1;
            }

            // Capability filters
            for cap in &query.capabilities {
                conditions.push(format!(
                    "e.manifest_capabilities_json LIKE ?{param_idx}"
                ));
                param_values.push(Box::new(format!("%\"{}\"%", cap.as_str())));
                param_idx += 1;
            }

            // Fitness filter
            if let Some(ref fitness) = query.min_fitness {
                conditions.push(format!(
                    "EXISTS (SELECT 1 FROM fitness_scores fs WHERE fs.entry_hash = e.hash AND fs.domain = ?{pi} AND fs.score >= ?{pi2})",
                    pi = param_idx, pi2 = param_idx + 1
                ));
                param_values.push(Box::new(fitness.domain.as_str().to_string()));
                param_values.push(Box::new(fitness.min_score));
                param_idx += 2;
            }

            // Generation filter
            if let Some(ref gen_filter) = query.generation {
                if let Some(min) = gen_filter.min {
                    conditions.push(format!("e.generation >= ?{param_idx}"));
                    param_values.push(Box::new(min.as_u32()));
                    param_idx += 1;
                }
                if let Some(max) = gen_filter.max {
                    conditions.push(format!("e.generation <= ?{param_idx}"));
                    param_values.push(Box::new(max.as_u32()));
                    let _ = param_idx; // final increment unused but keeps pattern consistent
                }
            }

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", conditions.join(" AND "))
            };

            let order = match query.order {
                SearchOrder::Newest => "e.created_at DESC",
                SearchOrder::Oldest => "e.created_at ASC",
                SearchOrder::HighestFitness => {
                    // Order by max fitness score in the queried domain
                    "COALESCE((SELECT MAX(fs.score) FROM fitness_scores fs WHERE fs.entry_hash = e.hash), 0) DESC"
                }
                SearchOrder::Relevance => {
                    if query.text.is_some() {
                        "rank" // FTS5 rank
                    } else {
                        "e.created_at DESC"
                    }
                }
            };

            let limit = query.limit.unwrap_or(100);

            let sql = format!(
                "SELECT e.hash, e.agent_name, e.version, e.generation, e.parentage_json,
                        e.change_rationale, e.created_at, e.manifest_description,
                        e.manifest_tools_json, e.manifest_capabilities_json
                 FROM entries e{fts_join}{where_clause}
                 ORDER BY {order}
                 LIMIT {limit}"
            );

            let mut stmt = conn.prepare(&sql).map_err(|e| RepositoryError::SearchExecution {
                source: Box::new(e),
            })?;

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|b| b.as_ref() as &dyn rusqlite::types::ToSql).collect();

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(row_to_header(row))
                })
                .map_err(|e| RepositoryError::SearchExecution {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::SearchExecution {
                    source: Box::new(e),
                })?;

            // Load fitness scores and tags for each header
            let mut headers = Vec::new();
            for mut header in rows {
                let hash_str = header.hash.as_str().to_string();
                header.fitness_scores = load_fitness(conn, &hash_str)?;
                header.tags = load_tags(conn, &hash_str)?;
                headers.push(header);
            }

            Ok(headers)
        })
        .await
    }

    async fn top_by_fitness(
        &self,
        domain: &FitnessDomain,
        limit: usize,
    ) -> Result<Vec<RepositoryEntryHeader>> {
        let domain_str = domain.as_str().to_string();
        self.with_conn(move |conn| {
            let sql = "SELECT e.hash, e.agent_name, e.version, e.generation, e.parentage_json,
                        e.change_rationale, e.created_at, e.manifest_description,
                        e.manifest_tools_json, e.manifest_capabilities_json
                 FROM entries e
                 JOIN fitness_scores fs ON fs.entry_hash = e.hash AND fs.domain = ?1
                 ORDER BY fs.score DESC
                 LIMIT ?2";

            let mut stmt = conn
                .prepare(sql)
                .map_err(|e| RepositoryError::SearchExecution {
                    source: Box::new(e),
                })?;

            let limit_i64 = limit as i64;
            let rows = stmt
                .query_map(params![domain_str, limit_i64], |row| Ok(row_to_header(row)))
                .map_err(|e| RepositoryError::SearchExecution {
                    source: Box::new(e),
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| RepositoryError::SearchExecution {
                    source: Box::new(e),
                })?;

            let mut headers = Vec::new();
            for mut header in rows {
                let hash_str = header.hash.as_str().to_string();
                header.fitness_scores = load_fitness(conn, &hash_str)?;
                header.tags = load_tags(conn, &hash_str)?;
                headers.push(header);
            }

            Ok(headers)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Internal row helpers
// ---------------------------------------------------------------------------

/// Helper trait for optional query results.
trait OptionalExt<T> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn load_headers(
    conn: &Connection,
    where_suffix: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Vec<RepositoryEntryHeader>> {
    let sql = format!(
        "SELECT e.hash, e.agent_name, e.version, e.generation, e.parentage_json,
                e.change_rationale, e.created_at, e.manifest_description,
                e.manifest_tools_json, e.manifest_capabilities_json
         FROM entries e {where_suffix}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
    let rows = stmt
        .query_map(params, |row| Ok(row_to_header(row)))
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;

    let mut headers = Vec::new();
    for mut header in rows {
        let hash_str = header.hash.as_str().to_string();
        header.fitness_scores = load_fitness(conn, &hash_str)?;
        header.tags = load_tags(conn, &hash_str)?;
        headers.push(header);
    }
    Ok(headers)
}

fn load_entry(
    conn: &Connection,
    where_suffix: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Option<RepositoryEntry>> {
    let sql = format!(
        "SELECT hash, agent_name, version, generation, parentage_json,
                source_json, change_rationale, created_at
         FROM entries WHERE {where_suffix}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;

    let result = stmt
        .query_row(params, |row| {
            let hash: String = row.get(0)?;
            let name: String = row.get(1)?;
            let version: u32 = row.get(2)?;
            let generation: u32 = row.get(3)?;
            let parentage_json: String = row.get(4)?;
            let source_json: String = row.get(5)?;
            let rationale: String = row.get(6)?;
            let created_at: String = row.get(7)?;
            Ok((
                hash,
                name,
                version,
                generation,
                parentage_json,
                source_json,
                rationale,
                created_at,
            ))
        })
        .optional()
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;

    let Some((hash, name, version, generation, parentage_json, source_json, rationale, created_at)) =
        result
    else {
        return Ok(None);
    };

    let hash: ContentHash = hash.parse().map_err(|e| RepositoryError::StorageRead {
        source: Box::new(e),
    })?;
    let hash_str = hash.as_str().to_string();

    let fitness_scores = load_fitness(conn, &hash_str)?;
    let tags = load_tags(conn, &hash_str)?;

    Ok(Some(RepositoryEntry {
        hash,
        version_ref: VersionRef {
            name: name.parse().map_err(|e| RepositoryError::StorageRead {
                source: Box::new(e),
            })?,
            version: Version::new(version).map_err(|e| RepositoryError::StorageRead {
                source: Box::new(e),
            })?,
        },
        source: serde_json::from_str(&source_json).map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?,
        parentage: serde_json::from_str(&parentage_json).map_err(|e| {
            RepositoryError::StorageRead {
                source: Box::new(e),
            }
        })?,
        generation: Generation::new(generation),
        change_rationale: ChangeRationale::new(rationale).map_err(|e| {
            RepositoryError::StorageRead {
                source: Box::new(e),
            }
        })?,
        created_at: Timestamp::new(created_at),
        fitness_scores,
        tags,
    }))
}

fn load_fitness(conn: &Connection, hash: &str) -> Result<Vec<FitnessScore>> {
    let mut stmt = conn
        .prepare(
            "SELECT domain, score, recorded_at FROM fitness_scores WHERE entry_hash = ?1 ORDER BY recorded_at DESC",
        )
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
    let scores = stmt
        .query_map(params![hash], |row| {
            let domain: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            let recorded_at: String = row.get(2)?;
            Ok(FitnessScore {
                domain: FitnessDomain::new(domain),
                score,
                recorded_at: Timestamp::new(recorded_at),
            })
        })
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
    Ok(scores)
}

fn load_tags(conn: &Connection, hash: &str) -> Result<Vec<Tag>> {
    let mut stmt = conn
        .prepare("SELECT tag FROM tags WHERE entry_hash = ?1 ORDER BY tag")
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
    let tags = stmt
        .query_map(params![hash], |row| {
            let tag: String = row.get(0)?;
            Ok(Tag::new(tag))
        })
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
    Ok(tags)
}

fn row_to_ancestry_node(row: &rusqlite::Row<'_>) -> AncestryNode {
    let hash: String = row.get(0).unwrap();
    let generation: u32 = row.get(1).unwrap();
    let parentage_json: String = row.get(2).unwrap();
    AncestryNode {
        hash: hash.parse().unwrap(),
        generation: Generation::new(generation),
        parentage: serde_json::from_str(&parentage_json).unwrap_or(Parentage::Original),
    }
}

fn row_to_header(row: &rusqlite::Row<'_>) -> RepositoryEntryHeader {
    let hash: String = row.get(0).unwrap();
    let name: String = row.get(1).unwrap();
    let version: u32 = row.get(2).unwrap();
    let generation: u32 = row.get(3).unwrap();
    let parentage_json: String = row.get(4).unwrap();
    let rationale: String = row.get(5).unwrap();
    let created_at: String = row.get(6).unwrap();
    let description: Option<String> = row.get(7).unwrap();
    let tools_json: String = row.get(8).unwrap();
    let caps_json: String = row.get(9).unwrap();

    let tools: Vec<String> = serde_json::from_str(&tools_json).unwrap_or_default();
    let capabilities: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();

    RepositoryEntryHeader {
        hash: hash.parse().unwrap(),
        version_ref: VersionRef {
            name: name.parse().unwrap(),
            version: Version::new(version).unwrap(),
        },
        parentage: serde_json::from_str(&parentage_json).unwrap_or(Parentage::Original),
        generation: Generation::new(generation),
        change_rationale: ChangeRationale::new(rationale)
            .unwrap_or_else(|_| ChangeRationale::new("(unknown)").unwrap()),
        created_at: Timestamp::new(created_at),
        fitness_scores: vec![], // Loaded separately
        tags: vec![],           // Loaded separately
        description,
        tools,
        capabilities,
    }
}
