//! SurrealDB-backed unified repository store.
//!
//! A single store implements metadata, lineage, search, and blob operations.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
};

use async_trait::async_trait;
use serde_json::Value;
use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};

use crate::{
    entry::{
        ChangeRationale, NewEntry, RepositoryEntry, RepositoryEntryHeader, SourceBundle, Tag,
        Timestamp,
    },
    error::{RepositoryError, Result},
    ids::{AgentName, ContentHash, Generation, LineageEdgeId, Version, VersionRef},
    lineage::{
        AncestryNode, EdgeDescription, LineageEdge, LineageKind, LineageSubgraph, Parentage,
    },
    search::{LineageRelation, SearchOrder, SearchQuery},
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
};

type LineageAdjacency = HashMap<ContentHash, Vec<(ContentHash, LineageKind)>>;

const NS: &str = "baml";
const DB_NAME: &str = "repository";

const TBL_ENTRIES: &str = "entries";
const TBL_TAGS: &str = "tags";
const TBL_EDGES: &str = "lineage_edges";
const TBL_BLOBS: &str = "blobs";

const SCHEMA_QUERIES: &[&str] = &[
    "DEFINE TABLE IF NOT EXISTS entries SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS hash ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS agent_name ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS version ON entries TYPE int",
    "DEFINE FIELD IF NOT EXISTS generation ON entries TYPE int",
    "DEFINE FIELD IF NOT EXISTS parentage_json ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS source_json ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS change_rationale ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS created_at ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS manifest_description ON entries TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS manifest_tools_json ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS manifest_capabilities_json ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS manifest_text ON entries TYPE string",
    "DEFINE FIELD IF NOT EXISTS source_text ON entries TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_entries_hash ON entries FIELDS hash UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_entries_name_version ON entries FIELDS agent_name, version UNIQUE",
    "DEFINE TABLE IF NOT EXISTS tags SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS entry_hash ON tags TYPE string",
    "DEFINE FIELD IF NOT EXISTS tag ON tags TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_tag_unique ON tags FIELDS entry_hash, tag UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_tag_lookup ON tags FIELDS tag",
    "DEFINE TABLE IF NOT EXISTS lineage_edges SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS id ON lineage_edges TYPE string",
    "DEFINE FIELD IF NOT EXISTS source_hash ON lineage_edges TYPE string",
    "DEFINE FIELD IF NOT EXISTS target_hash ON lineage_edges TYPE string",
    "DEFINE FIELD IF NOT EXISTS kind ON lineage_edges TYPE string",
    "DEFINE FIELD IF NOT EXISTS description ON lineage_edges TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_edge_id ON lineage_edges FIELDS id UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_edge_source ON lineage_edges FIELDS source_hash",
    "DEFINE INDEX IF NOT EXISTS idx_edge_target ON lineage_edges FIELDS target_hash",
    "DEFINE TABLE IF NOT EXISTS blobs SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS hash ON blobs TYPE string",
    "DEFINE FIELD IF NOT EXISTS data_hex ON blobs TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_blob_hash ON blobs FIELDS hash UNIQUE",
];

fn map_surreal_write(e: surrealdb::Error) -> RepositoryError {
    RepositoryError::StorageWrite {
        source: Box::new(e),
    }
}

fn map_surreal_read(e: surrealdb::Error) -> RepositoryError {
    RepositoryError::StorageRead {
        source: Box::new(e),
    }
}

fn decode_err(msg: impl Into<String>) -> RepositoryError {
    RepositoryError::StorageRead {
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            msg.into(),
        )),
    }
}

fn get_required_str<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| decode_err(format!("entries.{field} missing")))
}

fn get_optional_str(row: &Value, field: &str) -> Option<String> {
    row.get(field).and_then(Value::as_str).map(str::to_string)
}

fn get_required_u32(row: &Value, field: &str) -> Result<u32> {
    row.get(field)
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .ok_or_else(|| decode_err(format!("entries.{field} missing")))
}

fn build_lineage_adjacency(edges: &[LineageEdge]) -> (LineageAdjacency, LineageAdjacency) {
    let mut descendants_map: LineageAdjacency = HashMap::new();
    let mut ancestors_map: LineageAdjacency = HashMap::new();
    for e in edges {
        descendants_map
            .entry(e.source.clone())
            .or_default()
            .push((e.target.clone(), e.kind.clone()));
        ancestors_map
            .entry(e.target.clone())
            .or_default()
            .push((e.source.clone(), e.kind.clone()));
    }
    (descendants_map, ancestors_map)
}

fn bfs_reachable(
    start: &ContentHash,
    adjacency: &LineageAdjacency,
    kind: Option<&LineageKind>,
    max_depth: Option<u32>,
) -> HashSet<ContentHash> {
    let mut seen = HashSet::new();
    let mut q: VecDeque<(ContentHash, u32)> = VecDeque::new();
    q.push_back((start.clone(), 0));

    while let Some((cur, depth)) = q.pop_front() {
        if let Some(max) = max_depth
            && depth >= max
        {
            continue;
        }
        if let Some(nexts) = adjacency.get(&cur) {
            for (next, edge_kind) in nexts {
                if let Some(k) = kind
                    && edge_kind != k
                {
                    continue;
                }
                if seen.insert(next.clone()) {
                    q.push_back((next.clone(), depth + 1));
                }
            }
        }
    }
    seen
}

/// Decode a SurrealDB record-id string back to the UUID stored at CREATE time.
///
/// Rows come back as `lineage_edges:\`<uuid>\``; the backticks are SurrealDB's
/// escaping for record ids whose local part contains hyphens. Accept a bare
/// UUID too, so historical / in-memory rows still parse.
fn parse_edge_id(raw: &str) -> Option<uuid::Uuid> {
    let after_prefix = raw.strip_prefix(&format!("{TBL_EDGES}:")).unwrap_or(raw);
    let trimmed = after_prefix.trim_matches('`');
    uuid::Uuid::parse_str(trimmed).ok()
}

fn parent_hashes_from(hash: &ContentHash, edges: &[LineageEdge]) -> Vec<ContentHash> {
    edges
        .iter()
        .filter(|e| &e.target == hash)
        .map(|e| e.source.clone())
        .collect()
}

fn child_hashes_from(hash: &ContentHash, edges: &[LineageEdge]) -> Vec<ContentHash> {
    edges
        .iter()
        .filter(|e| &e.source == hash)
        .map(|e| e.target.clone())
        .collect()
}

fn influence_target_hashes_from(hash: &ContentHash, edges: &[LineageEdge]) -> Vec<ContentHash> {
    edges
        .iter()
        .filter(|e| &e.source == hash && matches!(e.kind, LineageKind::Influence))
        .map(|e| e.target.clone())
        .collect()
}

fn ancestor_hashes_from(
    hash: &ContentHash,
    edges: &[LineageEdge],
    max_depth: u32,
) -> Vec<ContentHash> {
    let (_, ancestors_map) = build_lineage_adjacency(edges);
    bfs_reachable(hash, &ancestors_map, None, Some(max_depth))
        .into_iter()
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

fn build_manifest_text(source: &SourceBundle) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = source.manifest.name() {
        parts.push(name.to_string());
    }
    if let Some(version) = source.manifest.version() {
        parts.push(version.to_string());
    }
    if let Some(description) = source.manifest.description() {
        parts.push(description.to_string());
    }
    parts.extend(source.manifest.tools().into_iter().map(str::to_string));
    parts.extend(
        source
            .manifest
            .capabilities()
            .into_iter()
            .map(str::to_string),
    );
    parts.extend(source.manifest.tags().into_iter().map(str::to_string));
    parts.join("\n")
}

fn build_source_text(source: &SourceBundle) -> String {
    let mut parts: Vec<String> = Vec::new();
    for file in &source.ts_sources {
        parts.push(file.path.as_str().to_string());
        parts.push(file.content.as_str().to_string());
    }
    for file in &source.baml_sources {
        parts.push(file.path.as_str().to_string());
        parts.push(file.content.as_str().to_string());
    }
    parts.join("\n")
}

fn row_contains_text(row: &Value, needle: &str) -> bool {
    let manifest_text = row
        .get("manifest_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if manifest_text.to_ascii_lowercase().contains(needle) {
        return true;
    }
    let source_text = row
        .get("source_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    source_text.to_ascii_lowercase().contains(needle)
}

pub struct SurrealStore {
    db: Arc<Surreal<Db>>,
}

impl SurrealStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path.as_ref().to_string_lossy().as_ref())
            .await
            .map_err(map_surreal_write)?;
        db.use_ns(NS)
            .use_db(DB_NAME)
            .await
            .map_err(map_surreal_write)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    pub async fn open_in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await.map_err(map_surreal_write)?;
        db.use_ns(NS)
            .use_db(DB_NAME)
            .await
            .map_err(map_surreal_write)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    pub async fn init_schema(&self) -> Result<()> {
        for stmt in SCHEMA_QUERIES {
            self.db.query(*stmt).await.map_err(map_surreal_write)?;
        }
        Ok(())
    }

    async fn entry_exists(&self, hash: &ContentHash) -> Result<bool> {
        Ok(self.get_by_hash(hash).await?.is_some())
    }

    async fn row_by_hash(&self, hash: &ContentHash) -> Result<Option<Value>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_ENTRIES} WHERE hash = $hash LIMIT 1"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        Ok(rows.into_iter().next())
    }

    async fn all_entry_rows(&self) -> Result<Vec<Value>> {
        let mut resp = self
            .db
            .query(format!("SELECT * FROM {TBL_ENTRIES}"))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        Ok(rows)
    }

    async fn tags_for_hash(&self, hash: &ContentHash) -> Result<Vec<Tag>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT tag FROM {TBL_TAGS} WHERE entry_hash = $hash ORDER BY tag ASC"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        Ok(rows
            .iter()
            .filter_map(|r| r.get("tag").and_then(Value::as_str))
            .map(|s| Tag::new(s.to_string()))
            .collect())
    }

    async fn header_from_row(&self, row: &Value) -> Result<RepositoryEntryHeader> {
        let hash_str = get_required_str(row, "hash")?;
        let hash: ContentHash = hash_str
            .parse()
            .map_err(|e| decode_err(format!("invalid hash: {e}")))?;
        let agent_name_str = get_required_str(row, "agent_name")?;
        let name: AgentName = agent_name_str
            .parse()
            .map_err(|e| decode_err(format!("invalid agent_name: {e}")))?;
        let version_num = get_required_u32(row, "version")?;
        let version =
            Version::new(version_num).map_err(|e| decode_err(format!("invalid version: {e}")))?;
        let generation = Generation::new(get_required_u32(row, "generation")?);
        let parentage_json = get_required_str(row, "parentage_json")?;
        let parentage: Parentage = serde_json::from_str(parentage_json)
            .map_err(|e| decode_err(format!("invalid parentage_json: {e}")))?;
        let rationale = get_required_str(row, "change_rationale")?;
        let change_rationale = ChangeRationale::new(rationale.to_string())
            .map_err(|_| decode_err("entries.change_rationale empty"))?;
        let created_at = Timestamp::new(get_required_str(row, "created_at")?.to_string());
        let description = get_optional_str(row, "manifest_description");
        let tools_json = row
            .get("manifest_tools_json")
            .and_then(Value::as_str)
            .unwrap_or("[]");
        let capabilities_json = row
            .get("manifest_capabilities_json")
            .and_then(Value::as_str)
            .unwrap_or("[]");
        let tools: Vec<String> = serde_json::from_str(tools_json)
            .map_err(|e| decode_err(format!("invalid manifest_tools_json: {e}")))?;
        let capabilities: Vec<String> = serde_json::from_str(capabilities_json)
            .map_err(|e| decode_err(format!("invalid manifest_capabilities_json: {e}")))?;

        Ok(RepositoryEntryHeader {
            hash: hash.clone(),
            version_ref: VersionRef { name, version },
            parentage,
            generation,
            change_rationale,
            created_at,
            tags: self.tags_for_hash(&hash).await?,
            description,
            tools,
            capabilities,
        })
    }

    async fn entry_from_row(&self, row: &Value) -> Result<RepositoryEntry> {
        let header = self.header_from_row(row).await?;
        let source_json = get_required_str(row, "source_json")?;
        let source: SourceBundle = serde_json::from_str(source_json)
            .map_err(|e| decode_err(format!("invalid source_json: {e}")))?;
        Ok(RepositoryEntry {
            hash: header.hash,
            version_ref: header.version_ref,
            source,
            parentage: header.parentage,
            generation: header.generation,
            change_rationale: header.change_rationale,
            created_at: header.created_at,
            tags: header.tags,
        })
    }

    async fn all_edges(&self) -> Result<Vec<LineageEdge>> {
        let mut resp = self
            .db
            .query(format!("SELECT * FROM {TBL_EDGES}"))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let mut out = Vec::new();
        for row in rows {
            let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
            let source = row
                .get("source_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let target = row
                .get("target_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or_default();
            let desc = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Ok(source_hash) = source.parse::<ContentHash>() else {
                tracing::warn!(
                    row_id = id,
                    source_hash = source,
                    event = "lineage_edge_row_skipped",
                    reason = "invalid source_hash"
                );
                continue;
            };
            let Ok(target_hash) = target.parse::<ContentHash>() else {
                tracing::warn!(
                    row_id = id,
                    target_hash = target,
                    event = "lineage_edge_row_skipped",
                    reason = "invalid target_hash"
                );
                continue;
            };
            let kind = match kind {
                "fork" => LineageKind::Fork,
                "influence" => LineageKind::Influence,
                other => {
                    tracing::warn!(
                        row_id = id,
                        kind = other,
                        event = "lineage_edge_row_skipped",
                        reason = "unknown kind"
                    );
                    continue;
                }
            };
            let Ok(description) = EdgeDescription::new(desc.to_string()) else {
                tracing::warn!(
                    row_id = id,
                    event = "lineage_edge_row_skipped",
                    reason = "empty description"
                );
                continue;
            };
            // A malformed edge id destroys edge identity — idempotent writes
            // and client-side caches cannot reconcile — so surface corruption
            // rather than fabricating a fresh UUID.
            //
            // SurrealDB returns the record id as `lineage_edges:`<uuid>``; the
            // CREATE path writes the UUID into the record-id slot, so strip
            // the table prefix and any id-escape backticks before parsing.
            let edge_uuid = parse_edge_id(id)
                .ok_or_else(|| decode_err(format!("invalid lineage_edges.id {id:?}")))?;
            out.push(LineageEdge {
                id: LineageEdgeId::from_uuid(edge_uuid),
                source: source_hash,
                target: target_hash,
                kind,
                description,
            });
        }
        Ok(out)
    }

    /// Hydrate a batch of ancestry nodes in a single query.
    ///
    /// Unlike [`header_from_row`], this only projects the columns an
    /// [`AncestryNode`] actually needs, avoiding per-node tag lookups — lineage
    /// traversals can be large, so the N+1 matters.
    async fn ancestry_nodes_for_hashes(&self, hashes: &[ContentHash]) -> Result<Vec<AncestryNode>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let hash_strings: Vec<String> = hashes.iter().map(|h| h.as_str().to_string()).collect();
        let mut resp = self
            .db
            .query(format!(
                "SELECT hash, generation, parentage_json FROM {TBL_ENTRIES} WHERE hash IN $hashes"
            ))
            .bind(("hashes", hash_strings))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let mut nodes = Vec::with_capacity(rows.len());
        for row in &rows {
            let hash: ContentHash = get_required_str(row, "hash")?
                .parse()
                .map_err(|e| decode_err(format!("invalid hash: {e}")))?;
            let generation = Generation::new(get_required_u32(row, "generation")?);
            let parentage: Parentage =
                serde_json::from_str(get_required_str(row, "parentage_json")?)
                    .map_err(|e| decode_err(format!("invalid parentage_json: {e}")))?;
            nodes.push(AncestryNode {
                hash,
                generation,
                parentage,
            });
        }
        Ok(nodes)
    }

    fn filter_by_name(&self, headers: &mut Vec<RepositoryEntryHeader>, query: &SearchQuery) {
        if let Some(name) = &query.name {
            headers.retain(|h| &h.version_ref.name == name);
        }
    }

    fn filter_by_tags(&self, headers: &mut Vec<RepositoryEntryHeader>, query: &SearchQuery) {
        for tag in &query.tags {
            headers.retain(|h| h.tags.iter().any(|t| t.as_str() == tag.as_str()));
        }
    }

    fn filter_by_tools(&self, headers: &mut Vec<RepositoryEntryHeader>, query: &SearchQuery) {
        for tool in &query.tools {
            headers.retain(|h| h.tools.iter().any(|t| t == tool.as_str()));
        }
    }

    fn filter_by_capabilities(
        &self,
        headers: &mut Vec<RepositoryEntryHeader>,
        query: &SearchQuery,
    ) {
        for cap in &query.capabilities {
            headers.retain(|h| h.capabilities.iter().any(|c| c == cap.as_str()));
        }
    }

    fn filter_by_generation(&self, headers: &mut Vec<RepositoryEntryHeader>, query: &SearchQuery) {
        if let Some(gf) = &query.generation {
            if let Some(min) = gf.min {
                headers.retain(|h| h.generation.as_u32() >= min.as_u32());
            }
            if let Some(max) = gf.max {
                headers.retain(|h| h.generation.as_u32() <= max.as_u32());
            }
        }
    }

    async fn filter_by_lineage(
        &self,
        headers: &mut Vec<RepositoryEntryHeader>,
        query: &SearchQuery,
    ) -> Result<()> {
        let Some(lineage) = &query.lineage else {
            return Ok(());
        };
        let edges = self.all_edges().await?;
        let (descendants_map, ancestors_map) = build_lineage_adjacency(&edges);

        let relation_hashes: HashSet<ContentHash> = match &lineage.relation {
            LineageRelation::DescendantOf { ancestor, kind } => {
                bfs_reachable(ancestor, &descendants_map, kind.as_ref(), None)
            }
            LineageRelation::AncestorOf { descendant, kind } => {
                bfs_reachable(descendant, &ancestors_map, kind.as_ref(), None)
            }
        };
        headers.retain(|h| relation_hashes.contains(&h.hash));
        Ok(())
    }

    fn apply_order(&self, headers: &mut [RepositoryEntryHeader], query: &SearchQuery) {
        match query.order {
            SearchOrder::Newest => {
                headers.sort_by(|a, b| b.created_at.as_str().cmp(a.created_at.as_str()))
            }
            SearchOrder::Oldest => {
                headers.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()))
            }
            SearchOrder::Relevance => {
                headers.sort_by(|a, b| b.created_at.as_str().cmp(a.created_at.as_str()));
            }
        }
    }

    fn apply_limit(&self, headers: &mut Vec<RepositoryEntryHeader>, query: &SearchQuery) {
        if let Some(limit) = query.limit {
            headers.truncate(limit);
        }
    }
}

#[async_trait]
impl BlobStore for SurrealStore {
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        self.db
            .query(format!(
                "UPSERT {TBL_BLOBS} SET hash = $hash, data_hex = $data_hex WHERE hash = $hash"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .bind(("data_hex", encode_hex(data)))
            .await
            .map_err(map_surreal_write)?;
        Ok(())
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT data_hex FROM {TBL_BLOBS} WHERE hash = $hash LIMIT 1"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(hex) = rows
            .first()
            .and_then(|r| r.get("data_hex"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        Ok(decode_hex(hex))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT hash FROM {TBL_BLOBS} WHERE hash = $hash LIMIT 1"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        Ok(!rows.is_empty())
    }

    async fn delete(&self, hash: &ContentHash) -> Result<()> {
        self.db
            .query(format!("DELETE FROM {TBL_BLOBS} WHERE hash = $hash"))
            .bind(("hash", hash.as_str().to_string()))
            .await
            .map_err(map_surreal_write)?;
        Ok(())
    }
}

#[async_trait]
impl MetadataStore for SurrealStore {
    async fn insert_entry(&self, entry: &NewEntry) -> Result<RepositoryEntry> {
        let name = entry.name.as_str().to_string();
        let mut resp = self
            .db
            .query(format!(
                "SELECT version FROM {TBL_ENTRIES} WHERE agent_name = $name ORDER BY version DESC LIMIT 1"
            ))
            .bind(("name", name.clone()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let next_version = rows
            .first()
            .and_then(|r| r.get("version").and_then(Value::as_u64))
            .map(|v| v as u32 + 1)
            .unwrap_or(1);

        // Repository publish path always assigns the next version in manifest
        // and computes the content hash from that canonical source bundle.
        let source_for_storage = entry.source.with_manifest_version(next_version);
        let hash = source_for_storage.compute_hash();
        let hash_str = hash.as_str().to_string();

        let mut dup_resp = self
            .db
            .query(format!(
                "SELECT agent_name, version FROM {TBL_ENTRIES} WHERE hash = $hash LIMIT 1"
            ))
            .bind(("hash", hash_str.clone()))
            .await
            .map_err(map_surreal_read)?;
        let dup_rows: Vec<Value> = dup_resp.take(0).map_err(map_surreal_read)?;
        if let Some(row) = dup_rows.first() {
            let existing_name = row
                .get("agent_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .parse()
                .map_err(|e| decode_err(format!("invalid existing agent_name: {e}")))?;
            let existing_version =
                Version::new(row.get("version").and_then(Value::as_u64).unwrap_or(1) as u32)
                    .map_err(|e| decode_err(format!("invalid existing version: {e}")))?;
            return Err(RepositoryError::DuplicateHash {
                hash,
                existing_name,
                existing_version,
            });
        }

        let version = Version::new(next_version).map_err(|e| decode_err(format!("{e}")))?;
        let parentage_json = serde_json::to_string(&entry.parentage)
            .map_err(|e| decode_err(format!("parentage serialization failed: {e}")))?;
        let source_json = serde_json::to_string(&source_for_storage)
            .map_err(|e| decode_err(format!("source serialization failed: {e}")))?;
        let created_at = crate::service::chrono_now();
        let description = source_for_storage
            .manifest
            .description()
            .map(str::to_string);
        let tools_json = serde_json::to_string(
            &source_for_storage
                .manifest
                .tools()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        let capabilities_json = serde_json::to_string(
            &source_for_storage
                .manifest
                .capabilities()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string());
        let manifest_text = build_manifest_text(&source_for_storage);
        let source_text = build_source_text(&source_for_storage);

        let _ = self
            .db
            .query(format!(
                "CREATE {TBL_ENTRIES} SET \
                    hash = $hash, \
                    agent_name = $name, \
                    version = $version, \
                    generation = $generation, \
                    parentage_json = $parentage_json, \
                    source_json = $source_json, \
                    change_rationale = $change_rationale, \
                    created_at = $created_at, \
                    manifest_description = $manifest_description, \
                    manifest_tools_json = $manifest_tools_json, \
                    manifest_capabilities_json = $manifest_capabilities_json, \
                    manifest_text = $manifest_text, \
                    source_text = $source_text"
            ))
            .bind(("hash", hash_str))
            .bind(("name", name))
            .bind(("version", next_version as i64))
            .bind(("generation", entry.generation.as_u32() as i64))
            .bind(("parentage_json", parentage_json))
            .bind(("source_json", source_json))
            .bind((
                "change_rationale",
                entry.change_rationale.as_str().to_string(),
            ))
            .bind(("created_at", created_at.as_str().to_string()))
            .bind(("manifest_description", description))
            .bind(("manifest_tools_json", tools_json))
            .bind(("manifest_capabilities_json", capabilities_json))
            .bind(("manifest_text", manifest_text))
            .bind(("source_text", source_text))
            .await
            .map_err(map_surreal_write)?;

        for tag in &entry.tags {
            self.db
                .query(format!(
                    "CREATE {TBL_TAGS} SET entry_hash = $hash, tag = $tag"
                ))
                .bind(("hash", hash.as_str().to_string()))
                .bind(("tag", tag.as_str().to_string()))
                .await
                .map_err(map_surreal_write)?;
        }

        Ok(RepositoryEntry {
            hash: hash.clone(),
            version_ref: VersionRef {
                name: entry.name.clone(),
                version,
            },
            source: source_for_storage,
            parentage: entry.parentage.clone(),
            generation: entry.generation,
            change_rationale: entry.change_rationale.clone(),
            created_at,
            tags: entry.tags.clone(),
        })
    }

    async fn get_by_hash(&self, hash: &ContentHash) -> Result<Option<RepositoryEntry>> {
        let Some(row) = self.row_by_hash(hash).await? else {
            return Ok(None);
        };
        Ok(Some(self.entry_from_row(&row).await?))
    }

    async fn get_by_version(
        &self,
        name: &AgentName,
        version: Version,
    ) -> Result<Option<RepositoryEntry>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_ENTRIES} WHERE agent_name = $name AND version = $version LIMIT 1"
            ))
            .bind(("name", name.as_str().to_string()))
            .bind(("version", version.as_u32() as i64))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(self.entry_from_row(row).await?))
    }

    async fn get_latest(&self, name: &AgentName) -> Result<Option<RepositoryEntry>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_ENTRIES} WHERE agent_name = $name ORDER BY version DESC LIMIT 1"
            ))
            .bind(("name", name.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(self.entry_from_row(row).await?))
    }

    async fn resolve_hash(&self, version_ref: &VersionRef) -> Result<Option<ContentHash>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT hash FROM {TBL_ENTRIES} WHERE agent_name = $name AND version = $version LIMIT 1"
            ))
            .bind(("name", version_ref.name.as_str().to_string()))
            .bind(("version", version_ref.version.as_u32() as i64))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(hash_str) = rows
            .first()
            .and_then(|r| r.get("hash"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let hash = hash_str
            .parse::<ContentHash>()
            .map_err(|e| decode_err(format!("invalid stored hash: {e}")))?;
        Ok(Some(hash))
    }

    async fn list_versions(&self, name: &AgentName) -> Result<Vec<RepositoryEntryHeader>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_ENTRIES} WHERE agent_name = $name ORDER BY version DESC"
            ))
            .bind(("name", name.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(self.header_from_row(&row).await?);
        }
        Ok(out)
    }

    async fn list_agents(&self) -> Result<Vec<AgentName>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT agent_name FROM {TBL_ENTRIES} GROUP BY agent_name"
            ))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(name) = row.get("agent_name").and_then(Value::as_str) {
                out.push(
                    name.parse::<AgentName>()
                        .map_err(|e| decode_err(format!("invalid stored agent name: {e}")))?,
                );
            }
        }
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(out)
    }

    async fn add_tag(&self, hash: &ContentHash, tag: Tag) -> Result<()> {
        if !self.entry_exists(hash).await? {
            return Err(RepositoryError::EntryNotFoundByHash { hash: hash.clone() });
        }
        let mut resp = self
            .db
            .query(format!(
                "SELECT tag FROM {TBL_TAGS} WHERE entry_hash = $hash AND tag = $tag LIMIT 1"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .bind(("tag", tag.as_str().to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        if rows.is_empty() {
            self.db
                .query(format!(
                    "CREATE {TBL_TAGS} SET entry_hash = $hash, tag = $tag"
                ))
                .bind(("hash", hash.as_str().to_string()))
                .bind(("tag", tag.as_str().to_string()))
                .await
                .map_err(map_surreal_write)?;
        }
        Ok(())
    }

    async fn remove_tag(&self, hash: &ContentHash, tag: &Tag) -> Result<()> {
        self.db
            .query(format!(
                "DELETE FROM {TBL_TAGS} WHERE entry_hash = $hash AND tag = $tag"
            ))
            .bind(("hash", hash.as_str().to_string()))
            .bind(("tag", tag.as_str().to_string()))
            .await
            .map_err(map_surreal_write)?;
        Ok(())
    }
}

#[async_trait]
impl LineageStore for SurrealStore {
    async fn record_edges(&self, edges: &[LineageEdge]) -> Result<()> {
        for edge in edges {
            self.db
                .query(format!(
                    "CREATE {TBL_EDGES} SET id = $id, source_hash = $source_hash, target_hash = $target_hash, kind = $kind, description = $description"
                ))
                .bind(("id", edge.id.as_str().to_string()))
                .bind(("source_hash", edge.source.as_str().to_string()))
                .bind(("target_hash", edge.target.as_str().to_string()))
                .bind((
                    "kind",
                    match edge.kind {
                        LineageKind::Fork => "fork",
                        LineageKind::Influence => "influence",
                    }
                    .to_string(),
                ))
                .bind(("description", edge.description.as_str().to_string()))
                .await
                .map_err(map_surreal_write)?;
        }
        Ok(())
    }

    async fn parents(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        self.ancestry_nodes_for_hashes(&parent_hashes_from(hash, &edges))
            .await
    }

    async fn children(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        self.ancestry_nodes_for_hashes(&child_hashes_from(hash, &edges))
            .await
    }

    async fn ancestors(&self, hash: &ContentHash, max_depth: u32) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        let mut nodes = self
            .ancestry_nodes_for_hashes(&ancestor_hashes_from(hash, &edges, max_depth))
            .await?;
        nodes.sort_by_key(|a| a.generation.as_u32());
        Ok(nodes)
    }

    async fn influenced_by(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        self.ancestry_nodes_for_hashes(&influence_target_hashes_from(hash, &edges))
            .await
    }

    async fn subgraph(&self, hash: &ContentHash, ancestor_depth: u32) -> Result<LineageSubgraph> {
        // Fetch edges once; derive ancestor + descendant sets in memory, then
        // hydrate both with a single batched node query.
        let edges = self.all_edges().await?;
        let ancestor_hashes = ancestor_hashes_from(hash, &edges, ancestor_depth);
        let descendant_hashes = child_hashes_from(hash, &edges);

        let union: Vec<ContentHash> = ancestor_hashes
            .iter()
            .chain(descendant_hashes.iter())
            .cloned()
            .collect();
        let node_by_hash: HashMap<ContentHash, AncestryNode> = self
            .ancestry_nodes_for_hashes(&union)
            .await?
            .into_iter()
            .map(|n| (n.hash.clone(), n))
            .collect();

        let mut ancestors: Vec<AncestryNode> = ancestor_hashes
            .iter()
            .filter_map(|h| node_by_hash.get(h).cloned())
            .collect();
        ancestors.sort_by_key(|a| a.generation.as_u32());
        let descendants: Vec<AncestryNode> = descendant_hashes
            .iter()
            .filter_map(|h| node_by_hash.get(h).cloned())
            .collect();

        let node_hashes: HashSet<ContentHash> = ancestors
            .iter()
            .map(|n| n.hash.clone())
            .chain(descendants.iter().map(|n| n.hash.clone()))
            .chain(std::iter::once(hash.clone()))
            .collect();
        let edges = edges
            .into_iter()
            .filter(|e| node_hashes.contains(&e.source) && node_hashes.contains(&e.target))
            .collect();
        Ok(LineageSubgraph {
            root: hash.clone(),
            ancestors,
            descendants,
            edges,
        })
    }
}

#[async_trait]
impl SearchStore for SurrealStore {
    async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>> {
        let mut rows = self.all_entry_rows().await?;
        if let Some(text) = &query.text {
            let needle = text.as_str().to_ascii_lowercase();
            rows.retain(|row| row_contains_text(row, &needle));
        }
        let mut headers = Vec::new();
        for row in rows {
            headers.push(self.header_from_row(&row).await?);
        }

        self.filter_by_name(&mut headers, query);
        self.filter_by_tags(&mut headers, query);
        self.filter_by_tools(&mut headers, query);
        self.filter_by_capabilities(&mut headers, query);
        self.filter_by_generation(&mut headers, query);
        self.filter_by_lineage(&mut headers, query).await?;
        self.apply_order(&mut headers, query);
        self.apply_limit(&mut headers, query);
        Ok(headers)
    }
}
