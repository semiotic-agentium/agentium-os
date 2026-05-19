//! SurrealDB-backed unified repository store.
//!
//! A single store implements metadata, lineage, search, and blob operations.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::Arc,
};

use async_trait::async_trait;
use baml_rt_tools::mcp_snapshot::{McpApprovalState, McpServerSnapshot};
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
    mcp::{
        McpRegistryServer, McpRegistryServerVersion, McpRegistryToolVersion,
        compute_snapshot_digest,
    },
    search::{LineageRelation, SearchOrder, SearchQuery},
    storage::{BlobStore, LineageStore, McpRegistryStore, MetadataStore, SearchStore},
};

type LineageAdjacency = HashMap<ContentHash, Vec<(ContentHash, LineageKind)>>;

const NS: &str = "baml";
const DB_NAME: &str = "repository";

const TBL_ENTRIES: &str = "entries";
const TBL_TAGS: &str = "tags";
const TBL_EDGES: &str = "lineage_edges";
const TBL_BLOBS: &str = "blobs";
const TBL_MCP_SERVERS: &str = "mcp_servers";
const TBL_MCP_SERVER_VERSIONS: &str = "mcp_server_versions";
const TBL_MCP_TOOL_VERSIONS: &str = "mcp_tool_versions";
const TBL_MCP_SNAPSHOT_BLOBS: &str = "mcp_snapshot_blobs";

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
    "DEFINE TABLE IF NOT EXISTS mcp_servers SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS server_id ON mcp_servers TYPE string",
    "DEFINE FIELD IF NOT EXISTS tenant_id ON mcp_servers TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS display_name ON mcp_servers TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS latest_version ON mcp_servers TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS created_at ON mcp_servers TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_mcp_server_id ON mcp_servers FIELDS server_id UNIQUE",
    "DEFINE TABLE IF NOT EXISTS mcp_server_versions SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS server_id ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS version ON mcp_server_versions TYPE int",
    "DEFINE FIELD IF NOT EXISTS snapshot_digest ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS server_config_digest ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS server_identity_digest ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS tools_digest ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS protocol_version ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS transport_json ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS secret_refs_json ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS sandbox_profile ON mcp_server_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS approval_state ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS owner ON mcp_server_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS reviewed_at ON mcp_server_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS expires_at ON mcp_server_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS created_at ON mcp_server_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS stale_at ON mcp_server_versions TYPE option<string>",
    "DEFINE INDEX IF NOT EXISTS idx_mcp_server_version_unique ON mcp_server_versions FIELDS server_id, version UNIQUE",
    "DEFINE TABLE IF NOT EXISTS mcp_tool_versions SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS server_id ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS server_version ON mcp_tool_versions TYPE int",
    "DEFINE FIELD IF NOT EXISTS platform_tool_name ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS mcp_tool_name ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS input_schema_digest ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS output_mode_json ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS access_level ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS approval_state ON mcp_tool_versions TYPE string",
    "DEFINE FIELD IF NOT EXISTS owner ON mcp_tool_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS reviewed_at ON mcp_tool_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS opaque_fallback_reason ON mcp_tool_versions TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS tool_json ON mcp_tool_versions TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_mcp_tool_version_unique ON mcp_tool_versions FIELDS server_id, server_version, platform_tool_name UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_mcp_tool_platform_name ON mcp_tool_versions FIELDS platform_tool_name",
    "DEFINE TABLE IF NOT EXISTS mcp_snapshot_blobs SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS snapshot_digest ON mcp_snapshot_blobs TYPE string",
    "DEFINE FIELD IF NOT EXISTS snapshot_json ON mcp_snapshot_blobs TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_mcp_snapshot_digest ON mcp_snapshot_blobs FIELDS snapshot_digest UNIQUE",
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

fn encode_json<T: serde::Serialize>(value: &T, field: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| decode_err(format!("{field} serialization failed: {e}")))
}

fn decode_json<T: for<'de> serde::Deserialize<'de>>(value: &str, field: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| decode_err(format!("invalid {field}: {e}")))
}

fn parse_mcp_approval_state(value: &str) -> Result<McpApprovalState> {
    match value {
        "pending" => Ok(McpApprovalState::Pending),
        "approved" => Ok(McpApprovalState::Approved),
        "rejected" => Ok(McpApprovalState::Rejected),
        "stale" => Ok(McpApprovalState::Stale),
        other => Err(decode_err(format!("invalid MCP approval state: {other}"))),
    }
}

fn mcp_approval_state_str(value: McpApprovalState) -> &'static str {
    match value {
        McpApprovalState::Pending => "pending",
        McpApprovalState::Approved => "approved",
        McpApprovalState::Rejected => "rejected",
        McpApprovalState::Stale => "stale",
    }
}

fn parse_tool_access(value: &str) -> Result<baml_rt_tools::tools::ToolAccess> {
    match value {
        "read" => Ok(baml_rt_tools::tools::ToolAccess::Read),
        "write" => Ok(baml_rt_tools::tools::ToolAccess::Write),
        "delete" => Ok(baml_rt_tools::tools::ToolAccess::Delete),
        other => Err(decode_err(format!(
            "invalid MCP tool access level: {other}"
        ))),
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
    let source_text = row
        .get("source_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let haystack = format!("{manifest_text}\n{source_text}").to_ascii_lowercase();
    haystack.contains(needle)
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
        let version_num = get_required_u32(row, "version");
        let version_num = version_num?;
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
        let tools: Vec<String> = serde_json::from_str(tools_json).unwrap_or_default();
        let capabilities: Vec<String> = serde_json::from_str(capabilities_json).unwrap_or_default();

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
                continue;
            };
            let Ok(target_hash) = target.parse::<ContentHash>() else {
                continue;
            };
            let kind = match kind {
                "fork" => LineageKind::Fork,
                "influence" => LineageKind::Influence,
                _ => continue,
            };
            let Ok(description) = EdgeDescription::new(desc.to_string()) else {
                continue;
            };
            out.push(LineageEdge {
                id: LineageEdgeId::from_uuid(
                    uuid::Uuid::parse_str(id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
                ),
                source: source_hash,
                target: target_hash,
                kind,
                description,
            });
        }
        Ok(out)
    }

    async fn node_for_hash(&self, hash: &ContentHash) -> Result<Option<AncestryNode>> {
        let Some(row) = self.row_by_hash(hash).await? else {
            return Ok(None);
        };
        let header = self.header_from_row(&row).await?;
        Ok(Some(AncestryNode {
            hash: header.hash,
            generation: header.generation,
            parentage: header.parentage,
        }))
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
            let _ = self
                .db
                .query(format!(
                    "CREATE {TBL_TAGS} SET entry_hash = $hash, tag = $tag"
                ))
                .bind(("hash", hash.as_str().to_string()))
                .bind(("tag", tag.as_str().to_string()))
                .await;
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
        let mut out = Vec::new();
        for edge in edges.iter().filter(|e| &e.target == hash) {
            if let Some(node) = self.node_for_hash(&edge.source).await? {
                out.push(node);
            }
        }
        Ok(out)
    }

    async fn children(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        let mut out = Vec::new();
        for edge in edges.iter().filter(|e| &e.source == hash) {
            if let Some(node) = self.node_for_hash(&edge.target).await? {
                out.push(node);
            }
        }
        Ok(out)
    }

    async fn ancestors(&self, hash: &ContentHash, max_depth: u32) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        let (_, ancestors_map) = build_lineage_adjacency(&edges);
        let visited = bfs_reachable(hash, &ancestors_map, None, Some(max_depth));
        let mut out = Vec::new();
        for ancestor_hash in visited {
            if let Some(node) = self.node_for_hash(&ancestor_hash).await? {
                out.push(node);
            }
        }
        out.sort_by_key(|a| a.generation.as_u32());
        Ok(out)
    }

    async fn influenced_by(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>> {
        let edges = self.all_edges().await?;
        let mut out = Vec::new();
        for edge in edges
            .iter()
            .filter(|e| &e.source == hash && matches!(e.kind, LineageKind::Influence))
        {
            if let Some(node) = self.node_for_hash(&edge.target).await? {
                out.push(node);
            }
        }
        Ok(out)
    }

    async fn subgraph(&self, hash: &ContentHash, ancestor_depth: u32) -> Result<LineageSubgraph> {
        let ancestors = self.ancestors(hash, ancestor_depth).await?;
        let descendants = self.children(hash).await?;
        let node_hashes: HashSet<ContentHash> = ancestors
            .iter()
            .map(|n| n.hash.clone())
            .chain(descendants.iter().map(|n| n.hash.clone()))
            .chain(std::iter::once(hash.clone()))
            .collect();
        let edges = self
            .all_edges()
            .await?
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
impl McpRegistryStore for SurrealStore {
    async fn list_mcp_servers(&self) -> Result<Vec<McpRegistryServer>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_MCP_SERVERS} ORDER BY server_id ASC"
            ))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        rows.iter().map(row_to_mcp_server).collect()
    }

    async fn put_mcp_snapshot(
        &self,
        snapshot: &McpServerSnapshot,
    ) -> Result<McpRegistryServerVersion> {
        let server_id = snapshot.server_id.clone();
        let mut resp = self
            .db
            .query(format!(
                "SELECT version FROM {TBL_MCP_SERVER_VERSIONS} WHERE server_id = $server_id ORDER BY version DESC LIMIT 1"
            ))
            .bind(("server_id", server_id.clone()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let next_version = rows
            .first()
            .and_then(|r| r.get("version").and_then(Value::as_u64))
            .map(|v| v as u32 + 1)
            .unwrap_or(1);

        let created_at = crate::service::chrono_now().as_str().to_string();
        let snapshot_digest = compute_snapshot_digest(snapshot);
        let snapshot_json = encode_json(snapshot, "mcp_snapshot")?;
        let transport_json = serde_json::to_value(&snapshot.transport)
            .map_err(|e| decode_err(format!("transport serialization failed: {e}")))?;
        let secret_refs_json = serde_json::to_value(&snapshot.secret_refs)
            .map_err(|e| decode_err(format!("secret_refs serialization failed: {e}")))?;

        self.db
            .query(format!(
                "UPSERT {TBL_MCP_SNAPSHOT_BLOBS} SET snapshot_digest = $snapshot_digest, snapshot_json = $snapshot_json WHERE snapshot_digest = $snapshot_digest"
            ))
            .bind(("snapshot_digest", snapshot_digest.to_string()))
            .bind(("snapshot_json", snapshot_json.clone()))
            .await
            .map_err(map_surreal_write)?;

        let _ = self
            .db
            .query(format!(
                "UPSERT {TBL_MCP_SERVERS} SET server_id = $server_id, latest_version = $latest_version, created_at = $created_at WHERE server_id = $server_id"
            ))
            .bind(("server_id", server_id.clone()))
            .bind(("latest_version", next_version as i64))
            .bind(("created_at", created_at.clone()))
            .await
            .map_err(map_surreal_write)?;

        let transport_json_string = encode_json(&transport_json, "transport_json")?;
        let secret_refs_json_string = encode_json(&secret_refs_json, "secret_refs_json")?;
        let approval_state = mcp_approval_state_str(snapshot.approval.state).to_string();
        let _ = self
            .db
            .query(format!(
                "CREATE {TBL_MCP_SERVER_VERSIONS} SET \
                    server_id = $server_id, \
                    version = $version, \
                    snapshot_digest = $snapshot_digest, \
                    server_config_digest = $server_config_digest, \
                    server_identity_digest = $server_identity_digest, \
                    tools_digest = $tools_digest, \
                    protocol_version = $protocol_version, \
                    transport_json = $transport_json, \
                    secret_refs_json = $secret_refs_json, \
                    sandbox_profile = $sandbox_profile, \
                    approval_state = $approval_state, \
                    owner = $owner, \
                    reviewed_at = $reviewed_at, \
                    expires_at = $expires_at, \
                    created_at = $created_at, \
                    stale_at = $stale_at"
            ))
            .bind(("server_id", server_id.clone()))
            .bind(("version", next_version as i64))
            .bind(("snapshot_digest", snapshot_digest.to_string()))
            .bind((
                "server_config_digest",
                snapshot.server_config_digest.to_string(),
            ))
            .bind((
                "server_identity_digest",
                snapshot.server_identity_digest.to_string(),
            ))
            .bind(("tools_digest", snapshot.tools_digest.to_string()))
            .bind(("protocol_version", snapshot.protocol_version.clone()))
            .bind(("transport_json", transport_json_string))
            .bind(("secret_refs_json", secret_refs_json_string))
            .bind(("sandbox_profile", snapshot.sandbox_profile.clone()))
            .bind(("approval_state", approval_state))
            .bind(("owner", snapshot.approval.owner.clone()))
            .bind(("reviewed_at", snapshot.approval.reviewed_at.clone()))
            .bind(("expires_at", snapshot.approval.expires_at.clone()))
            .bind(("created_at", created_at.clone()))
            .bind(("stale_at", Option::<String>::None))
            .await
            .map_err(map_surreal_write)?;

        for tool in &snapshot.tools {
            let output_mode_json = serde_json::to_value(&tool.output_mode)
                .map_err(|e| decode_err(format!("output_mode serialization failed: {e}")))?;
            let tool_json = serde_json::to_value(tool)
                .map_err(|e| decode_err(format!("tool serialization failed: {e}")))?;
            let _ = self
                .db
                .query(format!(
                    "CREATE {TBL_MCP_TOOL_VERSIONS} SET \
                        server_id = $server_id, \
                        server_version = $server_version, \
                        platform_tool_name = $platform_tool_name, \
                        mcp_tool_name = $mcp_tool_name, \
                        input_schema_digest = $input_schema_digest, \
                        output_mode_json = $output_mode_json, \
                        access_level = $access_level, \
                        approval_state = $approval_state, \
                        owner = $owner, \
                        reviewed_at = $reviewed_at, \
                        opaque_fallback_reason = $opaque_fallback_reason, \
                        tool_json = $tool_json"
                ))
                .bind(("server_id", server_id.clone()))
                .bind(("server_version", next_version as i64))
                .bind(("platform_tool_name", tool.platform_tool_name.clone()))
                .bind(("mcp_tool_name", tool.mcp_tool_name.clone()))
                .bind(("input_schema_digest", tool.input_schema_digest.to_string()))
                .bind((
                    "output_mode_json",
                    encode_json(&output_mode_json, "output_mode_json")?,
                ))
                .bind(("access_level", tool.access_level.as_str().to_string()))
                .bind((
                    "approval_state",
                    mcp_approval_state_str(tool.approval.state).to_string(),
                ))
                .bind(("owner", tool.approval.owner.clone()))
                .bind(("reviewed_at", tool.approval.reviewed_at.clone()))
                .bind((
                    "opaque_fallback_reason",
                    tool.opaque_fallback_reason.clone(),
                ))
                .bind(("tool_json", encode_json(&tool_json, "tool_json")?))
                .await
                .map_err(map_surreal_write)?;
        }

        Ok(McpRegistryServerVersion {
            server_id,
            version: next_version,
            snapshot_digest,
            server_config_digest: snapshot.server_config_digest,
            server_identity_digest: snapshot.server_identity_digest,
            tools_digest: snapshot.tools_digest,
            protocol_version: snapshot.protocol_version.clone(),
            transport_json,
            secret_refs_json,
            sandbox_profile: snapshot.sandbox_profile.clone(),
            approval_state: snapshot.approval.state,
            owner: snapshot.approval.owner.clone(),
            reviewed_at: snapshot.approval.reviewed_at.clone(),
            expires_at: snapshot.approval.expires_at.clone(),
            created_at,
            stale_at: None,
        })
    }

    async fn get_mcp_snapshot(
        &self,
        server_id: &str,
        version: u32,
    ) -> Result<Option<McpServerSnapshot>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT snapshot_digest FROM {TBL_MCP_SERVER_VERSIONS} WHERE server_id = $server_id AND version = $version LIMIT 1"
            ))
            .bind(("server_id", server_id.to_string()))
            .bind(("version", version as i64))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(snapshot_digest) = rows
            .first()
            .and_then(|r| r.get("snapshot_digest"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let mut blob_resp = self
            .db
            .query(format!(
                "SELECT snapshot_json FROM {TBL_MCP_SNAPSHOT_BLOBS} WHERE snapshot_digest = $snapshot_digest LIMIT 1"
            ))
            .bind(("snapshot_digest", snapshot_digest.to_string()))
            .await
            .map_err(map_surreal_read)?;
        let blob_rows: Vec<Value> = blob_resp.take(0).map_err(map_surreal_read)?;
        let Some(snapshot_json) = blob_rows
            .first()
            .and_then(|r| r.get("snapshot_json"))
            .and_then(Value::as_str)
        else {
            return Err(decode_err(format!(
                "MCP snapshot blob missing for digest {snapshot_digest}"
            )));
        };
        Ok(Some(decode_json(snapshot_json, "mcp_snapshot")?))
    }

    async fn get_latest_mcp_snapshot(&self, server_id: &str) -> Result<Option<McpServerSnapshot>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT latest_version FROM {TBL_MCP_SERVERS} WHERE server_id = $server_id LIMIT 1"
            ))
            .bind(("server_id", server_id.to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        let Some(version) = rows
            .first()
            .and_then(|r| r.get("latest_version"))
            .and_then(Value::as_u64)
        else {
            return Ok(None);
        };
        let version = version as u32;
        let mut version_resp = self
            .db
            .query(format!(
                "SELECT approval_state FROM {TBL_MCP_SERVER_VERSIONS} WHERE server_id = $server_id AND version = $version LIMIT 1"
            ))
            .bind(("server_id", server_id.to_string()))
            .bind(("version", version as i64))
            .await
            .map_err(map_surreal_read)?;
        let version_rows: Vec<Value> = version_resp.take(0).map_err(map_surreal_read)?;
        let approval_state = version_rows
            .first()
            .and_then(|r| r.get("approval_state"))
            .and_then(Value::as_str);
        if approval_state != Some("approved") {
            return Ok(None);
        }
        self.get_mcp_snapshot(server_id, version).await
    }

    async fn list_mcp_server_versions(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpRegistryServerVersion>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_MCP_SERVER_VERSIONS} WHERE server_id = $server_id ORDER BY version DESC"
            ))
            .bind(("server_id", server_id.to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        rows.iter().map(row_to_mcp_server_version).collect()
    }

    async fn find_mcp_tool(&self, platform_tool_name: &str) -> Result<Vec<McpRegistryToolVersion>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_MCP_TOOL_VERSIONS} WHERE platform_tool_name = $platform_tool_name ORDER BY server_version DESC"
            ))
            .bind(("platform_tool_name", platform_tool_name.to_string()))
            .await
            .map_err(map_surreal_read)?;
        let rows: Vec<Value> = resp.take(0).map_err(map_surreal_read)?;
        rows.iter().map(row_to_mcp_tool_version).collect()
    }

    async fn mark_mcp_version_stale(&self, server_id: &str, version: u32) -> Result<()> {
        let stale_at = crate::service::chrono_now().as_str().to_string();
        self.db
            .query(format!(
                "UPDATE {TBL_MCP_SERVER_VERSIONS} SET approval_state = $approval_state, stale_at = $stale_at WHERE server_id = $server_id AND version = $version"
            ))
            .bind(("approval_state", "stale".to_string()))
            .bind(("stale_at", stale_at.clone()))
            .bind(("server_id", server_id.to_string()))
            .bind(("version", version as i64))
            .await
            .map_err(map_surreal_write)?;
        self.db
            .query(format!(
                "UPDATE {TBL_MCP_TOOL_VERSIONS} SET approval_state = $approval_state WHERE server_id = $server_id AND server_version = $version"
            ))
            .bind(("approval_state", "stale".to_string()))
            .bind(("server_id", server_id.to_string()))
            .bind(("version", version as i64))
            .await
            .map_err(map_surreal_write)?;
        Ok(())
    }
}

fn row_to_mcp_server(row: &Value) -> Result<McpRegistryServer> {
    Ok(McpRegistryServer {
        server_id: get_required_str(row, "server_id")?.to_string(),
        tenant_id: get_optional_str(row, "tenant_id"),
        display_name: get_optional_str(row, "display_name"),
        created_at: get_required_str(row, "created_at")?.to_string(),
        latest_version: row
            .get("latest_version")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
    })
}

fn row_to_mcp_server_version(row: &Value) -> Result<McpRegistryServerVersion> {
    Ok(McpRegistryServerVersion {
        server_id: get_required_str(row, "server_id")?.to_string(),
        version: get_required_u32(row, "version")?,
        snapshot_digest: baml_rt_tools::mcp_snapshot::Digest::new(get_required_str(
            row,
            "snapshot_digest",
        )?),
        server_config_digest: baml_rt_tools::mcp_snapshot::Digest::new(get_required_str(
            row,
            "server_config_digest",
        )?),
        server_identity_digest: baml_rt_tools::mcp_snapshot::Digest::new(get_required_str(
            row,
            "server_identity_digest",
        )?),
        tools_digest: baml_rt_tools::mcp_snapshot::Digest::new(get_required_str(
            row,
            "tools_digest",
        )?),
        protocol_version: get_required_str(row, "protocol_version")?.to_string(),
        transport_json: decode_json(get_required_str(row, "transport_json")?, "transport_json")?,
        secret_refs_json: decode_json(
            get_required_str(row, "secret_refs_json")?,
            "secret_refs_json",
        )?,
        sandbox_profile: get_optional_str(row, "sandbox_profile"),
        approval_state: parse_mcp_approval_state(get_required_str(row, "approval_state")?)?,
        owner: get_optional_str(row, "owner"),
        reviewed_at: get_optional_str(row, "reviewed_at"),
        expires_at: get_optional_str(row, "expires_at"),
        created_at: get_required_str(row, "created_at")?.to_string(),
        stale_at: get_optional_str(row, "stale_at"),
    })
}

fn row_to_mcp_tool_version(row: &Value) -> Result<McpRegistryToolVersion> {
    Ok(McpRegistryToolVersion {
        server_id: get_required_str(row, "server_id")?.to_string(),
        server_version: get_required_u32(row, "server_version")?,
        platform_tool_name: get_required_str(row, "platform_tool_name")?.to_string(),
        mcp_tool_name: get_required_str(row, "mcp_tool_name")?.to_string(),
        input_schema_digest: baml_rt_tools::mcp_snapshot::Digest::new(get_required_str(
            row,
            "input_schema_digest",
        )?),
        output_mode_json: decode_json(
            get_required_str(row, "output_mode_json")?,
            "output_mode_json",
        )?,
        access_level: parse_tool_access(get_required_str(row, "access_level")?)?,
        approval_state: parse_mcp_approval_state(get_required_str(row, "approval_state")?)?,
        owner: get_optional_str(row, "owner"),
        reviewed_at: get_optional_str(row, "reviewed_at"),
        opaque_fallback_reason: get_optional_str(row, "opaque_fallback_reason"),
        tool_json: decode_json(get_required_str(row, "tool_json")?, "tool_json")?,
    })
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

#[cfg(test)]
mod tests {
    use baml_rt_tools::{
        mcp_snapshot::{
            ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpImportedTool, McpOutputMode,
            McpTransportRef, SecretRef, compute_tools_digest,
        },
        tools::ToolAccess,
    };
    use serde_json::json;

    use super::*;

    fn approved() -> ApprovalRecord {
        ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("operator@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        }
    }

    fn tool(name: &str, schema_digest: &str) -> McpImportedTool {
        McpImportedTool {
            platform_tool_name: format!("mcp/meteo/{name}"),
            mcp_tool_name: name.into(),
            description: Some(format!("{name} tool")),
            input_schema: json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
            input_schema_digest: Digest::new(schema_digest),
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: approved(),
            opaque_fallback_reason: None,
            annotations: serde_json::Value::Null,
        }
    }

    fn snapshot(schema_digest: &str) -> McpServerSnapshot {
        let tools = vec![tool("get_meteo", schema_digest)];
        McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: "meteo".into(),
            transport: McpTransportRef::Stdio {
                command_ref: "meteo-mcp".into(),
                args: vec!["--stdio".into()],
            },
            protocol_version: "2025-06-18".into(),
            server_info: Some(json!({ "name": "meteo" })),
            server_config_digest: Digest::new(
                "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            ),
            server_identity_digest: Digest::new(
                "sha256:3333333333333333333333333333333333333333333333333333333333333333",
            ),
            tools_digest: compute_tools_digest(&tools),
            secret_refs: vec![SecretRef {
                version: Some("1".into()),
                ..SecretRef::stdio_env("meteo/token")
            }],
            approval: approved(),
            sandbox_profile: Some("restricted".into()),
            tools,
        }
    }

    #[tokio::test]
    async fn mcp_snapshot_versions_round_trip() {
        let store = SurrealStore::open_in_memory().await.unwrap();
        let first =
            snapshot("sha256:11111111111111111111111111111111111111111111111111111111111111111");
        let inserted = store.put_mcp_snapshot(&first).await.unwrap();
        assert_eq!(inserted.server_id, "meteo");
        assert_eq!(inserted.version, 1);

        let read_back = store.get_mcp_snapshot("meteo", 1).await.unwrap().unwrap();
        assert_eq!(read_back, first);
        let latest = store
            .get_latest_mcp_snapshot("meteo")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest, first);

        let second =
            snapshot("sha256:11111111111111111111111111111111111111111111111111111111111111112");
        let inserted_second = store.put_mcp_snapshot(&second).await.unwrap();
        assert_eq!(inserted_second.version, 2);
        let versions = store.list_mcp_server_versions("meteo").await.unwrap();
        assert_eq!(
            versions.iter().map(|v| v.version).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let latest = store
            .get_latest_mcp_snapshot("meteo")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest, second);
    }

    #[tokio::test]
    async fn mcp_tool_lookup_and_stale_transition() {
        let store = SurrealStore::open_in_memory().await.unwrap();
        store
            .put_mcp_snapshot(&snapshot(
                "sha256:11111111111111111111111111111111111111111111111111111111111111111",
            ))
            .await
            .unwrap();

        let tools = store.find_mcp_tool("mcp/meteo/get_meteo").await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].approval_state, McpApprovalState::Approved);
        assert_eq!(tools[0].access_level, ToolAccess::Read);

        store.mark_mcp_version_stale("meteo", 1).await.unwrap();
        let versions = store.list_mcp_server_versions("meteo").await.unwrap();
        assert_eq!(versions[0].approval_state, McpApprovalState::Stale);
        assert!(versions[0].stale_at.is_some());
        let tools = store.find_mcp_tool("mcp/meteo/get_meteo").await.unwrap();
        assert_eq!(tools[0].approval_state, McpApprovalState::Stale);
        assert!(
            store
                .get_latest_mcp_snapshot("meteo")
                .await
                .unwrap()
                .is_none(),
            "latest lookup must not return a stale snapshot as approved"
        );
    }
}
