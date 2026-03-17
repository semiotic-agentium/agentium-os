//! SurrealDB-backed provenance store.
//!
//! Implements the same trait set as [`GraphqliteProvenanceStore`](crate::graphqlite_store::GraphqliteProvenanceStore):
//! - [`ProvenanceWriter`] + [`ProvenanceContextReader`]
//! - [`ProvenanceQueryApi`]
//! - [`A2aGraphStore`]
//! - [`ProvenancePlanningQuery`]
//! - [`ProvenanceOpsQuery`]
//!
//! **Not implemented**: [`GraphStore`](crate::GraphStore) — that trait is Cypher-specific.
//! SurrealDB callers use SurrealQL directly via the store's native methods.
//!
//! ## Concurrency model
//!
//! SurrealDB is async-first with native MVCC. No global mutex or dedicated worker thread
//! is needed (unlike GraphQLite which requires a serialized worker due to C extension
//! global mutable state).
//!
//! ## Storage architecture
//!
//! The store uses SurrealDB tables to model the provenance graph:
//!
//! | Table | Purpose |
//! |-------|---------|
//! | `prov_node` | All graph nodes (entities, activities, agents) with `label` + `props` |
//! | `prov_edge` | All graph edges (Used, WasGeneratedBy, etc.) with `rel_type` + `props` |
//! | `provenance_payload` | Payload side-table (prompt/result blobs, same contract as GraphQLite) |
//! | `a2a_task` | A2A task subgraph nodes |
//! | `a2a_message` | A2A task message nodes |
//! | `a2a_update` | A2A task update nodes |

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{AgentId, ContextId, EventId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_vocabulary::{
    A2aGraphStore, A2aGraphStoreResult, TaskSubgraphNode, TaskSubgraphUpdateNode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};
use tokio::sync::Mutex;

use crate::{
    error::{ProvenanceError, Result},
    events::ProvEventData,
    graph_model::GraphNodeLabel,
    mermaid_cache::MermaidCache,
    normalizer::{
        A2aRelationType, DefaultProvNormalizer, NormalizeContext, NormalizedProv, ProvNormalizer,
        task_entity_id_string, validate_event,
    },
    store::{
        ActivityRef, ArchiveRef, PayloadRef, PlanningIntentRecord, PlanningPlanRecord,
        PlanningPlanStepRecord, ProvenanceArchivePayload, ProvenanceArchiveRecord,
        ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
        ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse,
        ProvenanceOpsResource, ProvenancePlanningQuery, ProvenanceQueryApi,
        ProvenanceResponseProfile, ProvenanceWriter, ToolSessionPhase,
    },
    vocabulary::semantic_labels,
};

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

const NS: &str = "provenance";
const DB: &str = "store";

// Table names
const TBL_NODE: &str = "prov_node";
const TBL_EDGE: &str = "prov_edge";
const TBL_PAYLOAD: &str = "provenance_payload";
const TBL_A2A_TASK: &str = "a2a_task";
const TBL_A2A_MESSAGE: &str = "a2a_message";
const TBL_A2A_UPDATE: &str = "a2a_update";

// ---------------------------------------------------------------------------
// Record types for SurrealDB serde
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PayloadRecord {
    payload_id: String,
    event_id: String,
    activity_id: Option<String>,
    payload_kind: String,
    payload_json: String,
}

// ---------------------------------------------------------------------------
// Backend enum + builder
// ---------------------------------------------------------------------------

/// Backend strategy for SurrealDB provenance store.
///
/// Mirrors [`GraphqliteBackend`](crate::graphqlite_store::GraphqliteBackend) semantics.
#[derive(Clone, Debug)]
pub enum SurrealBackend {
    /// File-backed: SurrealKV embedded storage in a directory.
    /// One shared store per directory path.
    File(crate::surreal_config::SurrealStoreConfig),
    /// In-memory shared: one global store for the process.
    InMemoryShared,
    /// Fresh isolated in-memory store per call (for tests).
    InMemoryIsolated,
}

impl SurrealBackend {
    /// File-backed store at the given directory path.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(crate::surreal_config::SurrealStoreConfig::file(
            path.as_ref(),
        ))
    }

    /// In-memory store shared by all callers.
    pub fn in_memory_shared() -> Self {
        Self::InMemoryShared
    }

    /// Build a store from this backend config.
    pub async fn build_store(
        &self,
        mermaid_cache: Option<Arc<MermaidCache>>,
    ) -> Result<Arc<SurrealProvenanceStore>> {
        match self {
            SurrealBackend::File(config) => {
                get_or_init_file_store(&config.path, mermaid_cache).await
            }
            SurrealBackend::InMemoryShared => {
                get_or_init_shared_in_memory_store(mermaid_cache).await
            }
            SurrealBackend::InMemoryIsolated => build_in_memory_isolated_store(mermaid_cache).await,
        }
    }
}

/// Builder for the SurrealDB provenance store.
pub struct SurrealStoreBuilder {
    backend: Option<SurrealBackend>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

impl SurrealStoreBuilder {
    pub fn new() -> Self {
        Self {
            backend: None,
            mermaid_cache: None,
        }
    }

    /// File-backed store at the given directory path.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self {
            backend: Some(SurrealBackend::file(path)),
            mermaid_cache: None,
        }
    }

    /// In-memory store shared by all callers.
    pub fn in_memory() -> Self {
        Self {
            backend: Some(SurrealBackend::in_memory_shared()),
            mermaid_cache: None,
        }
    }

    /// Fresh isolated in-memory store (for tests).
    pub fn in_memory_isolated() -> Self {
        Self {
            backend: Some(SurrealBackend::InMemoryIsolated),
            mermaid_cache: None,
        }
    }

    /// Use an explicit backend.
    pub fn backend(backend: SurrealBackend) -> Self {
        Self {
            backend: Some(backend),
            mermaid_cache: None,
        }
    }

    /// Attach Mermaid cache for invalidation on add_event.
    pub fn with_mermaid_cache(mut self, cache: Arc<MermaidCache>) -> Self {
        self.mermaid_cache = Some(cache);
        self
    }

    /// Build the store.
    pub async fn build(self) -> Result<Arc<SurrealProvenanceStore>> {
        let backend = self.backend.ok_or_else(|| ProvenanceError::InvalidEvent {
            event_id: String::new(),
            reason: "SurrealStoreBuilder: no backend set".to_string(),
        })?;
        backend.build_store(self.mermaid_cache).await
    }
}

impl Default for SurrealStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Store caching (shared/file)
// ---------------------------------------------------------------------------

/// File-backed stores cached by canonicalized path.
static FILE_STORES: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<SurrealProvenanceStore>>>> =
    OnceLock::new();

/// Shared in-memory singleton.
static SHARED_IN_MEMORY_STORE: OnceLock<Mutex<Option<Arc<SurrealProvenanceStore>>>> =
    OnceLock::new();

async fn get_or_init_file_store(
    path: &std::path::Path,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let mutex = FILE_STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = mutex.lock().await;
    if let Some(store) = guard.get(path) {
        return Ok(store.clone());
    }
    let db = Surreal::new::<SurrealKv>(path.to_string_lossy().as_ref())
        .await
        .map_err(map_surreal_error)?;
    let store = init_store(db, mermaid_cache).await?;
    guard.insert(path.to_path_buf(), store.clone());
    Ok(store)
}

async fn get_or_init_shared_in_memory_store(
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let mutex = SHARED_IN_MEMORY_STORE.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;
    if let Some(store) = guard.as_ref() {
        return Ok(store.clone());
    }
    let store = build_in_memory_isolated_store(mermaid_cache).await?;
    *guard = Some(store.clone());
    Ok(store)
}

async fn build_in_memory_isolated_store(
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let db = Surreal::new::<Mem>(()).await.map_err(map_surreal_error)?;
    init_store(db, mermaid_cache).await
}

async fn init_store(
    db: Surreal<Db>,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    db.use_ns(NS).use_db(DB).await.map_err(map_surreal_error)?;
    init_schema(&db).await?;
    let store = SurrealProvenanceStore {
        db,
        normalizer: Arc::new(DefaultProvNormalizer::default()),
        mermaid_cache,
    };
    Ok(Arc::new(store))
}

async fn init_schema(db: &Surreal<Db>) -> Result<()> {
    // Define indexes for efficient queries.
    // prov_node: unique by node_id, indexed by label and common property lookups.
    let schema_queries = [
        // Node table: unique node_id, indexed by label
        format!("DEFINE INDEX IF NOT EXISTS idx_node_id ON {TBL_NODE} FIELDS node_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_label ON {TBL_NODE} FIELDS label"),
        // Indexes for common node property queries (context_id, task_id, event_id)
        format!("DEFINE INDEX IF NOT EXISTS idx_node_context ON {TBL_NODE} FIELDS props.a2a_context_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_task ON {TBL_NODE} FIELDS props.a2a_task_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_event ON {TBL_NODE} FIELDS props.a2a_event_id"),
        // Edge table: indexed by from/to and rel_type
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_from ON {TBL_EDGE} FIELDS from_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to ON {TBL_EDGE} FIELDS to_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_rel ON {TBL_EDGE} FIELDS rel_type"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_composite ON {TBL_EDGE} FIELDS from_id, rel_type, to_id UNIQUE"),
        // Payload table: unique payload_id, indexed by event_id and activity_id
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_id ON {TBL_PAYLOAD} FIELDS payload_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_event ON {TBL_PAYLOAD} FIELDS event_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity ON {TBL_PAYLOAD} FIELDS activity_id, payload_kind"),
        // A2A task table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_id ON {TBL_A2A_TASK} FIELDS task_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_ctx ON {TBL_A2A_TASK} FIELDS context_id"),
        // A2A message table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_id ON {TBL_A2A_MESSAGE} FIELDS msg_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_task ON {TBL_A2A_MESSAGE} FIELDS task_id, seq"),
        // A2A update table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_id ON {TBL_A2A_UPDATE} FIELDS update_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_task ON {TBL_A2A_UPDATE} FIELDS task_id, seq"),
        // Full-text search on payload_json for payload text search parity
        "DEFINE ANALYZER IF NOT EXISTS payload_analyzer TOKENIZERS blank, class FILTERS snowball(english)".to_string(),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_fts ON {TBL_PAYLOAD} FIELDS payload_json SEARCH ANALYZER payload_analyzer BM25"),
    ];
    for query in &schema_queries {
        db.query(query).await.map_err(map_surreal_error)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core store struct
// ---------------------------------------------------------------------------

/// SurrealDB-backed provenance store. Implements the same trait set as
/// [`GraphqliteProvenanceStore`](crate::graphqlite_store::GraphqliteProvenanceStore)
/// except for `GraphStore` (Cypher-specific).
pub struct SurrealProvenanceStore {
    db: Surreal<Db>,
    normalizer: Arc<dyn ProvNormalizer>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_surreal_error(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

fn payload_id_for(event_id: &str, payload_kind: &str) -> String {
    format!("payload:{event_id}:{payload_kind}")
}

fn archive_ref_for_payload(payload_id: &str) -> String {
    format!("prov:v1:payload:{payload_id}")
}

fn archive_ref_for_activity(activity_id: &str) -> String {
    format!("prov:v1:activity:{activity_id}")
}

fn event_id_to_timestamp_ms(event_id: &str) -> u64 {
    event_id
        .strip_prefix("prov-")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

fn event_order_key(event_id: &EventId) -> u128 {
    let digits: String = event_id
        .as_str()
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u128>().unwrap_or(0)
}

fn normalize_message_content(value: &Value) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

fn is_empty_object(value: &Value) -> bool {
    matches!(value, Value::Object(m) if m.is_empty())
}

fn has_meaningful_result(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

/// Reserved for conversation_context tool metadata extraction.
#[allow(dead_code)]
fn metadata_error(metadata: &Value) -> Option<Value> {
    let error = metadata.get("error")?;
    if has_meaningful_result(error) {
        Some(error.clone())
    } else {
        None
    }
}

fn is_step_completed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "done" | "step_completed" | "finished"
    )
}

fn decode_depends_on(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|value| value.as_array().cloned())
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn label_from_prov_type(prov_type: Option<&str>, default: &str) -> String {
    prov_type
        .map(|t| {
            // Strip namespace prefix if present (e.g. "a2a:LlmCall" → "LlmCall")
            t.rsplit_once(':')
                .map(|(_, suffix)| suffix)
                .unwrap_or(t)
                .to_string()
        })
        .unwrap_or_else(|| default.to_string())
}

// ---------------------------------------------------------------------------
// Payload extraction from events (shared with GraphQLite)
// ---------------------------------------------------------------------------

fn payload_records_from_event(event: &crate::events::ProvEvent) -> Vec<PayloadRecord> {
    let event_id = event.id().as_str().to_string();
    match event.data() {
        ProvEventData::LlmCallStarted { prompt, .. } => vec![PayloadRecord {
            payload_id: payload_id_for(&event_id, "llm_call"),
            event_id,
            activity_id: None,
            payload_kind: "llm_call".to_string(),
            payload_json: serde_json::to_string(prompt).unwrap_or_else(|_| "null".to_string()),
        }],
        ProvEventData::LlmCallCompleted {
            prompt, metadata, ..
        } => {
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&event_id, "llm_call"),
                event_id: event_id.clone(),
                activity_id: None,
                payload_kind: "llm_call".to_string(),
                payload_json: serde_json::to_string(prompt).unwrap_or_else(|_| "null".to_string()),
            }];
            let result = metadata.get("result").cloned();
            let error = metadata.get("error").cloned();
            let payload = match (result, error) {
                (Some(result), Some(error)) => {
                    serde_json::json!({ "result": result, "error": error })
                }
                (Some(result), None) => result,
                (None, Some(error)) => serde_json::json!({ "error": error }),
                (None, None) => Value::Null,
            };
            out.push(PayloadRecord {
                payload_id: payload_id_for(&event_id, "llm_result"),
                event_id,
                activity_id: None,
                payload_kind: "llm_result".to_string(),
                payload_json: serde_json::to_string(&payload)
                    .unwrap_or_else(|_| "null".to_string()),
            });
            out
        }
        ProvEventData::ToolCallStarted {
            tool_name,
            args,
            metadata,
            ..
        }
        | ProvEventData::ToolCallCompleted {
            tool_name,
            args,
            metadata,
            ..
        } => {
            let phase = metadata.get("phase").cloned().unwrap_or(Value::Null);
            let tool_call = serde_json::json!({
                "name": tool_name,
                "args": args,
                "phase": phase
            });
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&event_id, "tool_call"),
                event_id: event_id.clone(),
                activity_id: None,
                payload_kind: "tool_call".to_string(),
                payload_json: serde_json::to_string(&tool_call)
                    .unwrap_or_else(|_| "null".to_string()),
            }];
            if matches!(event.data(), ProvEventData::ToolCallCompleted { .. }) {
                let result = metadata.get("result").cloned();
                let error = metadata.get("error").cloned();
                let payload = match (result, error) {
                    (Some(result), Some(error)) => {
                        serde_json::json!({ "result": result, "error": error })
                    }
                    (Some(result), None) => result,
                    (None, Some(error)) => serde_json::json!({ "error": error }),
                    (None, None) => Value::Null,
                };
                out.push(PayloadRecord {
                    payload_id: payload_id_for(&event_id, "tool_result"),
                    event_id,
                    activity_id: None,
                    payload_kind: "tool_result".to_string(),
                    payload_json: serde_json::to_string(&payload)
                        .unwrap_or_else(|_| "null".to_string()),
                });
            }
            out
        }
        _ => Vec::new(),
    }
}

fn archive_payload_from_record(payload: PayloadRecord) -> Result<ProvenanceArchivePayload> {
    let payload_ref = PayloadRef(archive_ref_for_payload(&payload.payload_id));
    let activity_id = payload
        .activity_id
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            event_id: payload.event_id.clone(),
            reason: format!(
                "payload {} missing activity_id for kind {}",
                payload.payload_id, payload.payload_kind
            ),
        })?;
    let activity_ref = ActivityRef(archive_ref_for_activity(&activity_id));
    let payload_json = payload.payload_json;
    let parsed: Value =
        serde_json::from_str(&payload_json).unwrap_or_else(|_| Value::String(payload_json.clone()));
    match payload.payload_kind.as_str() {
        "llm_call" => Ok(ProvenanceArchivePayload::LlmCall {
            payload_ref,
            activity_ref,
            prompt_json: payload_json,
        }),
        "llm_result" => Ok(ProvenanceArchivePayload::LlmResult {
            payload_ref,
            activity_ref,
            result_json: payload_json,
        }),
        "tool_call" => {
            let tool_name = parsed
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let phase = parsed
                .get("phase")
                .and_then(Value::as_str)
                .map(str::to_string);
            let args = parsed.get("args").cloned().unwrap_or(Value::Null);
            let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "null".to_string());
            Ok(ProvenanceArchivePayload::ToolCall {
                payload_ref,
                activity_ref,
                tool_name,
                phase,
                args_json,
            })
        }
        "tool_result" => Ok(ProvenanceArchivePayload::ToolResult {
            payload_ref,
            activity_ref,
            result_json: payload_json,
        }),
        other => Err(ProvenanceError::InvalidEvent {
            event_id: payload.event_id.clone(),
            reason: format!("unsupported payload_kind for archive retrieval: {other}"),
        }),
    }
}

enum ParsedArchiveRef<'a> {
    PayloadId(&'a str),
    ActivityId(&'a str),
}

fn parse_archive_ref(archive_ref: &str) -> Option<ParsedArchiveRef<'_>> {
    if let Some(payload_id) = archive_ref.strip_prefix("prov:v1:payload:") {
        if payload_id.is_empty() {
            return None;
        }
        return Some(ParsedArchiveRef::PayloadId(payload_id));
    }
    if let Some(activity_id) = archive_ref.strip_prefix("prov:v1:activity:") {
        if activity_id.is_empty() {
            return None;
        }
        return Some(ParsedArchiveRef::ActivityId(activity_id));
    }
    None
}

// ---------------------------------------------------------------------------
// NormalizedProv → SurrealDB write
// ---------------------------------------------------------------------------

impl SurrealProvenanceStore {
    /// Write a normalized provenance document to SurrealDB.
    ///
    /// Translates entities, activities, agents → `prov_node` records,
    /// and relations → `prov_edge` records.
    async fn write_normalized(
        &self,
        normalized: &NormalizedProv,
        context_id: Option<&str>,
    ) -> Result<()> {
        // Collect label maps for nodes (mirrors cypher_build logic)
        let mut entity_labels = HashMap::new();
        for (id, entity) in normalized.document.entities() {
            let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
            entity_labels.insert(id.as_str().to_string(), label);
        }
        let mut activity_labels = HashMap::new();
        for (id, activity) in normalized.document.activities() {
            let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
            activity_labels.insert(id.as_str().to_string(), label);
        }
        let mut agent_labels = HashMap::new();
        for (id, agent) in normalized.document.agents() {
            let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
            agent_labels.insert(id.as_str().to_string(), label);
        }
        for (id, label) in &normalized.agent_labels {
            agent_labels
                .entry(id.clone())
                .or_insert_with(|| label.clone());
        }

        // Upsert entity nodes
        for (id, entity) in normalized.document.entities() {
            let label = entity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            let mut props = entity.attributes.clone();
            if let Some(ref pt) = entity.prov_type {
                props.insert("prov_type".to_string(), Value::String(pt.clone()));
            }
            // Use storage-safe underscore keys (same as GraphQLite)
            self.upsert_node(id.as_str(), label, &props).await?;
        }

        // Upsert activity nodes
        for (id, activity) in normalized.document.activities() {
            let label = activity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            let mut props = activity.attributes.clone();
            if let Some(ref pt) = activity.prov_type {
                props.insert("prov_type".to_string(), Value::String(pt.clone()));
            }
            if let Some(start) = activity.start_time_ms {
                props.insert("prov_startTime".to_string(), Value::from(start));
            }
            if let Some(end) = activity.end_time_ms {
                props.insert("prov_endTime".to_string(), Value::from(end));
            }
            self.upsert_node(id.as_str(), label, &props).await?;
        }

        // Upsert agent nodes
        for (id, agent) in normalized.document.agents() {
            let label = agent_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvAgent");
            let mut props = agent.attributes.clone();
            if let Some(ref pt) = agent.prov_type {
                props.insert("prov_type".to_string(), Value::String(pt.clone()));
            }
            self.upsert_node(id.as_str(), label, &props).await?;
        }

        // Insert edges: Used
        for (_, used) in normalized.document.used() {
            let mut edge_props: HashMap<String, Value> = HashMap::new();
            if let Some(ref role) = used.role {
                edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
            }
            let activity_label = activity_labels
                .get(used.activity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            let entity_label = entity_labels
                .get(used.entity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            self.upsert_edge(
                used.activity.as_str(),
                activity_label,
                "USED",
                used.entity.as_str(),
                entity_label,
                &edge_props,
            )
            .await?;
        }

        // Insert edges: WasGeneratedBy
        for (_, generated) in normalized.document.was_generated_by() {
            let edge_props: HashMap<String, Value> = HashMap::new();
            let entity_id = generated.entity.id();
            let entity_label = match &generated.entity {
                crate::types::ProvNodeRef::Entity(eid) => entity_labels
                    .get(eid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvEntity"),
                crate::types::ProvNodeRef::Activity(aid) => activity_labels
                    .get(aid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvActivity"),
                crate::types::ProvNodeRef::Agent(agid) => agent_labels
                    .get(agid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvAgent"),
            };
            let activity_label = activity_labels
                .get(generated.activity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            self.upsert_edge(
                entity_id,
                entity_label,
                "WAS_GENERATED_BY",
                generated.activity.as_str(),
                activity_label,
                &edge_props,
            )
            .await?;
        }

        // Insert edges: QualifiedGeneration
        for (_, generation) in normalized.document.qualified_generation() {
            let edge_props: HashMap<String, Value> = HashMap::new();
            let entity_id = generation.entity.id();
            let entity_label = match &generation.entity {
                crate::types::ProvNodeRef::Entity(eid) => entity_labels
                    .get(eid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvEntity"),
                crate::types::ProvNodeRef::Activity(aid) => activity_labels
                    .get(aid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvActivity"),
                crate::types::ProvNodeRef::Agent(agid) => agent_labels
                    .get(agid.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvAgent"),
            };
            let activity_label = activity_labels
                .get(generation.activity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            self.upsert_edge(
                entity_id,
                entity_label,
                "QUALIFIED_GENERATION",
                generation.activity.as_str(),
                activity_label,
                &edge_props,
            )
            .await?;
        }

        // Insert edges: WasAssociatedWith
        for (_, assoc) in normalized.document.was_associated_with() {
            let mut edge_props: HashMap<String, Value> = HashMap::new();
            if let Some(ref role) = assoc.role {
                edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
            }
            let activity_label = activity_labels
                .get(assoc.activity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            let agent_label = agent_labels
                .get(assoc.agent.as_str())
                .map(String::as_str)
                .unwrap_or("ProvAgent");
            self.upsert_edge(
                assoc.activity.as_str(),
                activity_label,
                "WAS_ASSOCIATED_WITH",
                assoc.agent.as_str(),
                agent_label,
                &edge_props,
            )
            .await?;
        }

        // Insert edges: WasDerivedFrom
        for (_, derived) in normalized.document.was_derived_from() {
            let mut edge_props: HashMap<String, Value> = HashMap::new();
            if let Some(ref pt) = derived.prov_type {
                edge_props.insert("prov_type".to_string(), Value::String(pt.clone()));
            }
            let generated_label = entity_labels
                .get(derived.generated_entity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            let used_label = entity_labels
                .get(derived.used_entity.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            self.upsert_edge(
                derived.generated_entity.as_str(),
                generated_label,
                "WAS_DERIVED_FROM",
                derived.used_entity.as_str(),
                used_label,
                &edge_props,
            )
            .await?;
        }

        // Insert derived relations (supersession edges for Intent/Plan)
        for relation in &normalized.derived_relations {
            let mut edge_props: HashMap<String, Value> = HashMap::new();
            for (k, v) in &relation.attributes {
                edge_props.insert(k.clone(), v.clone());
            }
            let (from_label, to_label) = match relation.relation {
                A2aRelationType::IntentReplacedBy | A2aRelationType::IntentRefinedBy => (
                    GraphNodeLabel::Intent.as_str(),
                    GraphNodeLabel::Intent.as_str(),
                ),
                A2aRelationType::PlanReplacedBy | A2aRelationType::PlanRefinedBy => {
                    (GraphNodeLabel::Plan.as_str(), GraphNodeLabel::Plan.as_str())
                }
                _ => continue,
            };
            let rel_type = match relation.relation {
                A2aRelationType::IntentReplacedBy | A2aRelationType::PlanReplacedBy => {
                    semantic_labels::WAS_REPLACED_BY
                }
                A2aRelationType::IntentRefinedBy | A2aRelationType::PlanRefinedBy => {
                    semantic_labels::WAS_REFINED_BY
                }
                _ => continue,
            };
            self.upsert_edge(
                relation.from.id(),
                from_label,
                rel_type,
                relation.to.id(),
                to_label,
                &edge_props,
            )
            .await?;
        }

        // Context scoping: create a Context node and SCOPED_TO edges
        if let Some(ctx_id) = context_id {
            let ctx_node_id = format!("context:{ctx_id}");
            let ctx_props: HashMap<String, Value> = HashMap::new();
            self.upsert_node(&ctx_node_id, "Context", &ctx_props)
                .await?;

            // Scope all nodes in this event to the context
            for (id, _) in normalized.document.entities() {
                let label = entity_labels
                    .get(id.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvEntity");
                self.upsert_edge(
                    id.as_str(),
                    label,
                    "SCOPED_TO",
                    &ctx_node_id,
                    "Context",
                    &HashMap::new(),
                )
                .await?;
            }
            for (id, _) in normalized.document.activities() {
                let label = activity_labels
                    .get(id.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvActivity");
                self.upsert_edge(
                    id.as_str(),
                    label,
                    "SCOPED_TO",
                    &ctx_node_id,
                    "Context",
                    &HashMap::new(),
                )
                .await?;
            }
            for (id, _) in normalized.document.agents() {
                let label = agent_labels
                    .get(id.as_str())
                    .map(String::as_str)
                    .unwrap_or("ProvAgent");
                self.upsert_edge(
                    id.as_str(),
                    label,
                    "SCOPED_TO",
                    &ctx_node_id,
                    "Context",
                    &HashMap::new(),
                )
                .await?;
            }
        }

        Ok(())
    }

    /// Upsert a single node into prov_node table.
    async fn upsert_node(
        &self,
        node_id: &str,
        label: &str,
        props: &HashMap<String, Value>,
    ) -> Result<()> {
        let safe_props = storage_safe_props(props);
        let props_value: Value =
            Value::Object(safe_props.into_iter().collect::<Map<String, Value>>());
        // Check existence, then update or create.
        let exists_query =
            format!("SELECT count() AS cnt FROM {TBL_NODE} WHERE node_id = $node_id GROUP ALL");
        let mut exists_resp = self
            .db
            .query(&exists_query)
            .bind(("node_id", node_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let exists_rows: Vec<Value> = exists_resp.take(0).map_err(map_surreal_error)?;
        let count = exists_rows
            .first()
            .and_then(|r| r.get("cnt").and_then(Value::as_i64))
            .unwrap_or(0);
        if count > 0 {
            // Merge props into existing record (Cypher MERGE semantics: update individual fields,
            // don't replace the entire props object).
            let mut set_clauses = vec!["label = $label".to_string()];
            let safe_props = storage_safe_props(props);
            for (i, (k, _)) in safe_props.iter().enumerate() {
                set_clauses.push(format!("props.{k} = $prop_{i}"));
            }
            let set_clause = set_clauses.join(", ");
            let update_query =
                format!("UPDATE {TBL_NODE} SET {set_clause} WHERE node_id = $node_id");
            let mut q = self
                .db
                .query(&update_query)
                .bind(("node_id", node_id.to_string()))
                .bind(("label", label.to_string()));
            for (i, (_, v)) in safe_props.iter().enumerate() {
                q = q.bind((format!("prop_{i}"), v.clone()));
            }
            q.await.map_err(map_surreal_error)?;
        } else {
            let create_query =
                format!("CREATE {TBL_NODE} SET node_id = $node_id, label = $label, props = $props");
            self.db
                .query(&create_query)
                .bind(("node_id", node_id.to_string()))
                .bind(("label", label.to_string()))
                .bind(("props", props_value))
                .await
                .map_err(map_surreal_error)?;
        }
        Ok(())
    }

    /// Upsert an edge into prov_edge table.
    async fn upsert_edge(
        &self,
        from_id: &str,
        from_label: &str,
        rel_type: &str,
        to_id: &str,
        to_label: &str,
        props: &HashMap<String, Value>,
    ) -> Result<()> {
        let safe_props = storage_safe_props(props);
        let props_value: Value =
            Value::Object(safe_props.into_iter().collect::<Map<String, Value>>());
        let exists_query = format!(
            "SELECT count() AS cnt FROM {TBL_EDGE} WHERE from_id = $from_id AND rel_type = $rel_type AND to_id = $to_id GROUP ALL"
        );
        let mut exists_resp = self
            .db
            .query(&exists_query)
            .bind(("from_id", from_id.to_string()))
            .bind(("rel_type", rel_type.to_string()))
            .bind(("to_id", to_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let exists_rows: Vec<Value> = exists_resp.take(0).map_err(map_surreal_error)?;
        let count = exists_rows
            .first()
            .and_then(|r| r.get("cnt").and_then(Value::as_i64))
            .unwrap_or(0);
        if count > 0 {
            let update_query = format!(
                "UPDATE {TBL_EDGE} SET from_label = $from_label, to_label = $to_label, props = $props WHERE from_id = $from_id AND rel_type = $rel_type AND to_id = $to_id"
            );
            self.db
                .query(&update_query)
                .bind(("from_id", from_id.to_string()))
                .bind(("from_label", from_label.to_string()))
                .bind(("to_id", to_id.to_string()))
                .bind(("to_label", to_label.to_string()))
                .bind(("rel_type", rel_type.to_string()))
                .bind(("props", props_value))
                .await
                .map_err(map_surreal_error)?;
        } else {
            let create_query = format!(
                "CREATE {TBL_EDGE} SET from_id = $from_id, from_label = $from_label, to_id = $to_id, to_label = $to_label, rel_type = $rel_type, props = $props"
            );
            self.db
                .query(&create_query)
                .bind(("from_id", from_id.to_string()))
                .bind(("from_label", from_label.to_string()))
                .bind(("to_id", to_id.to_string()))
                .bind(("to_label", to_label.to_string()))
                .bind(("rel_type", rel_type.to_string()))
                .bind(("props", props_value))
                .await
                .map_err(map_surreal_error)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Node queries
    // -----------------------------------------------------------------------

    /// Query nodes by label with optional property filter.
    /// Reserved for parity tests and Phase 2 query surfaces.
    #[allow(dead_code)]
    async fn query_nodes_by_label(
        &self,
        label: &str,
        filters: &[(&str, &str)],
    ) -> Result<Vec<Value>> {
        let mut where_clauses = vec!["label = $label".to_string()];
        for (i, (key, _)) in filters.iter().enumerate() {
            let safe_key = key.replace(':', "_");
            where_clauses.push(format!("props.{safe_key} = $filter_{i}"));
        }
        let where_clause = where_clauses.join(" AND ");
        let query = format!("SELECT * OMIT id FROM {TBL_NODE} WHERE {where_clause}");
        let mut q = self.db.query(&query).bind(("label", label.to_string()));
        for (i, (_, value)) in filters.iter().enumerate() {
            q = q.bind((format!("filter_{i}"), value.to_string()));
        }
        let mut response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows)
    }

    /// Get a single node by its node_id.
    async fn get_node(&self, node_id: &str) -> Result<Option<Value>> {
        let query = format!("SELECT * OMIT id FROM {TBL_NODE} WHERE node_id = $node_id LIMIT 1");
        let mut response = self
            .db
            .query(&query)
            .bind(("node_id", node_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows.into_iter().next())
    }

    /// Query edges by relationship type with optional from/to filters.
    async fn query_edges(
        &self,
        rel_type: &str,
        from_id: Option<&str>,
        to_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut where_clauses = vec!["rel_type = $rel_type".to_string()];
        if from_id.is_some() {
            where_clauses.push("from_id = $from_id".to_string());
        }
        if to_id.is_some() {
            where_clauses.push("to_id = $to_id".to_string());
        }
        let where_clause = where_clauses.join(" AND ");
        let query = format!("SELECT * OMIT id FROM {TBL_EDGE} WHERE {where_clause}");
        let mut q = self
            .db
            .query(&query)
            .bind(("rel_type", rel_type.to_string()));
        if let Some(fid) = from_id {
            q = q.bind(("from_id", fid.to_string()));
        }
        if let Some(tid) = to_id {
            q = q.bind(("to_id", tid.to_string()));
        }
        let mut response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Payload operations
    // -----------------------------------------------------------------------

    async fn upsert_payload(&self, payload: PayloadRecord) -> Result<()> {
        let exists_query = format!(
            "SELECT count() AS cnt FROM {TBL_PAYLOAD} WHERE payload_id = $payload_id GROUP ALL"
        );
        let mut exists_resp = self
            .db
            .query(&exists_query)
            .bind(("payload_id", payload.payload_id.clone()))
            .await
            .map_err(map_surreal_error)?;
        let exists_rows: Vec<Value> = exists_resp.take(0).map_err(map_surreal_error)?;
        let count = exists_rows
            .first()
            .and_then(|r| r.get("cnt").and_then(Value::as_i64))
            .unwrap_or(0);
        if count > 0 {
            let update_query = format!(
                "UPDATE {TBL_PAYLOAD} SET event_id = $event_id, activity_id = $activity_id, payload_kind = $payload_kind, payload_json = $payload_json WHERE payload_id = $payload_id"
            );
            self.db
                .query(&update_query)
                .bind(("payload_id", payload.payload_id))
                .bind(("event_id", payload.event_id))
                .bind(("activity_id", payload.activity_id))
                .bind(("payload_kind", payload.payload_kind))
                .bind(("payload_json", payload.payload_json))
                .await
                .map_err(map_surreal_error)?;
        } else {
            let create_query = format!(
                "CREATE {TBL_PAYLOAD} SET payload_id = $payload_id, event_id = $event_id, activity_id = $activity_id, payload_kind = $payload_kind, payload_json = $payload_json"
            );
            self.db
                .query(&create_query)
                .bind(("payload_id", payload.payload_id))
                .bind(("event_id", payload.event_id))
                .bind(("activity_id", payload.activity_id))
                .bind(("payload_kind", payload.payload_kind))
                .bind(("payload_json", payload.payload_json))
                .await
                .map_err(map_surreal_error)?;
        }
        Ok(())
    }

    async fn read_payload_by_id(&self, payload_id: &str) -> Result<Option<PayloadRecord>> {
        let query = format!(
            "SELECT payload_id, event_id, activity_id, payload_kind, payload_json FROM {TBL_PAYLOAD} WHERE payload_id = $payload_id LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("payload_id", payload_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<PayloadRecord> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows.into_iter().next())
    }

    async fn read_payload_by_event_kind(
        &self,
        event_id: &str,
        payload_kind: &str,
    ) -> Result<Option<PayloadRecord>> {
        let query = format!(
            "SELECT payload_id, event_id, activity_id, payload_kind, payload_json FROM {TBL_PAYLOAD} WHERE event_id = $event_id AND payload_kind = $payload_kind LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("event_id", event_id.to_string()))
            .bind(("payload_kind", payload_kind.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<PayloadRecord> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows.into_iter().next())
    }

    async fn read_payloads_by_activity(&self, activity_id: &str) -> Result<Vec<PayloadRecord>> {
        let query = format!(
            "SELECT payload_id, event_id, activity_id, payload_kind, payload_json FROM {TBL_PAYLOAD} WHERE activity_id = $activity_id ORDER BY payload_kind"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("activity_id", activity_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<PayloadRecord> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows)
    }

    /// Reserved for payload text search parity (ops query filter).
    #[allow(dead_code)]
    async fn search_payload_activity_ids(&self, query_text: &str) -> Result<Vec<String>> {
        let query = format!(
            "SELECT DISTINCT activity_id FROM {TBL_PAYLOAD} WHERE payload_json @@ $query_text AND activity_id IS NOT NONE"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("query_text", query_text.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                row.get("activity_id")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect())
    }
}

/// Convert property keys from colon-style (a2a:context_id) to underscore-style (a2a_context_id)
/// for storage-safe access in SurrealDB property paths.
fn storage_safe_props(props: &HashMap<String, Value>) -> HashMap<String, Value> {
    props
        .iter()
        .map(|(k, v)| {
            let safe_key = k.replace(':', "_");
            // Serialize nested objects/arrays to JSON strings for parity with GraphQLite
            let safe_value = match v {
                Value::Array(_) | Value::Object(_) => {
                    Value::String(serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()))
                }
                _ => v.clone(),
            };
            (safe_key, safe_value)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvenanceWriter for SurrealProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        validate_event(&event)?;
        self.enforce_step_completion_gate(&event).await?;
        let mut payload_records = payload_records_from_event(&event);
        let context = match event.task_id() {
            Some(tid) => {
                let task_agent_id = self.get_task_agent_id(tid).await?;
                NormalizeContext { task_agent_id }
            }
            None => NormalizeContext::default(),
        };
        let normalized = self
            .normalizer
            .normalize_with_context(&event, Some(&context))?;
        let context_id_opt = event.context_id_opt().map(|c| c.as_str().to_string());
        self.write_normalized(&normalized, context_id_opt.as_deref())
            .await?;

        if !payload_records.is_empty() {
            let activity_id = self
                .resolve_call_activity_id_for_event(event.id().as_str())
                .await?;
            for payload in &mut payload_records {
                if let Some(ref activity_id) = activity_id {
                    payload.activity_id = Some(activity_id.clone());
                }
                self.upsert_payload(payload.clone()).await?;
            }
        }

        if let (Some(cache), Some(ctx)) = (&self.mermaid_cache, context_id_opt.as_deref()) {
            cache.invalidate(ctx);
        }
        Ok(())
    }
}

#[async_trait]
impl ProvenanceContextReader for SurrealProvenanceStore {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        let ctx = context_id.as_str();
        let query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'Message' AND props.a2a_context_id = $ctx"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let event_id = props
                .get("a2a_event_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let message_id = props
                .get("a2a_message_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let role = props
                .get("a2a_role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
            let content_value = match &content_raw {
                Value::String(s) => {
                    serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
                }
                other => other.clone(),
            };
            let content = normalize_message_content(&content_value);
            if content.trim().is_empty() {
                continue;
            }
            messages.push(ProvenanceContextMessage {
                message_id: MessageId::from(message_id),
                timestamp_ms: event_id_to_timestamp_ms(event_id),
                role: role.to_string(),
                content: vec![content],
            });
        }
        messages.retain(|m| !m.content.iter().all(|c| c.trim().is_empty()));
        messages.sort_by_key(|m| m.timestamp_ms);
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if messages.len() > n {
                messages = messages.split_off(messages.len() - n);
            }
        }
        Ok(messages)
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let ctx = context_id.as_str();

        // Fetch message items
        let msg_query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'Message' AND props.a2a_context_id = $ctx"
        );
        let mut msg_response = self
            .db
            .query(&msg_query)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let msg_rows: Vec<Value> = msg_response.take(0).map_err(map_surreal_error)?;

        // Fetch tool call items (completed only)
        let tool_query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'ToolCall' AND props.a2a_context_id = $ctx AND props.a2a_activity_outcome = 'Success'"
        );
        let mut tool_response = self
            .db
            .query(&tool_query)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let tool_rows: Vec<Value> = tool_response.take(0).map_err(map_surreal_error)?;

        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in &msg_rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let event_id = props
                .get("a2a_event_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let role = props
                .get("a2a_role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
            let content_value = match &content_raw {
                Value::String(s) => {
                    serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
                }
                other => other.clone(),
            };
            let content = normalize_message_content(&content_value);
            if content.trim().is_empty() {
                continue;
            }
            items.push(ProvenanceConversationContextItem {
                timestamp_ms: event_id_to_timestamp_ms(event_id),
                event_id: EventId::from(event_id),
                role: role.to_string(),
                content: Value::String(content),
                source: "message".to_string(),
            });
        }

        for row in &tool_rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let event_id_str = props
                .get("a2a_event_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let tool_name = props
                .get("a2a_tool_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tool_call_payload = self
                .read_payload_by_event_kind(event_id_str, "tool_call")
                .await?;
            let tool_result_payload = self
                .read_payload_by_event_kind(event_id_str, "tool_result")
                .await?;

            let (args, phase) = if let Some(payload) = tool_call_payload {
                let parsed: Value =
                    serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                let args = parsed
                    .get("args")
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));
                let phase_label = parsed
                    .get("phase")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                (
                    args,
                    ToolSessionPhase::from_metadata(&serde_json::json!({ "phase": phase_label })),
                )
            } else {
                (
                    Value::Object(Map::new()),
                    ToolSessionPhase::from_metadata(&Value::Null),
                )
            };

            let (result, error) = if let Some(payload) = tool_result_payload {
                let parsed: Value =
                    serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                let result = parsed
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| parsed.clone());
                let error = parsed.get("error").cloned();
                (result, error)
            } else {
                (Value::Object(Map::new()), None)
            };

            let has_outcome = has_meaningful_result(&result) || error.is_some();
            let include_call = !matches!(
                phase,
                ToolSessionPhase::Open | ToolSessionPhase::Finish | ToolSessionPhase::Abort
            ) && (!is_empty_object(&args) || has_outcome);

            if include_call {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_to_timestamp_ms(event_id_str),
                    event_id: EventId::from(event_id_str),
                    role: "assistant".to_string(),
                    content: serde_json::json!({ "tool_call": { "name": tool_name, "args": args, "fsm_phase": phase.label() } }),
                    source: "tool_call".to_string(),
                });
            }

            if include_call && has_outcome {
                let mut content = serde_json::Map::new();
                content.insert("tool_name".to_string(), Value::String(tool_name.clone()));
                content.insert("fsm_phase".to_string(), Value::String(phase.label()));
                if has_meaningful_result(&result) {
                    content.insert("result".to_string(), result);
                }
                if let Some(error) = error {
                    content.insert("error".to_string(), error);
                }
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_to_timestamp_ms(event_id_str),
                    event_id: EventId::from(event_id_str),
                    role: "tool".to_string(),
                    content: Value::Object(content),
                    source: "tool_result".to_string(),
                });
            }
        }

        items.sort_by_key(|i| {
            (
                i.timestamp_ms,
                event_id_to_timestamp_ms(i.event_id.as_str()),
                i.source.clone(),
            )
        });
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if items.len() > n {
                items = items.split_off(items.len() - n);
            }
        }
        Ok(items)
    }
}

#[async_trait]
impl ProvenanceQueryApi for SurrealProvenanceStore {
    async fn query_context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        ProvenanceContextReader::context_messages(self, context_id, limit).await
    }

    async fn query_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        ProvenanceContextReader::conversation_context(self, context_id, limit).await
    }
}

// ---------------------------------------------------------------------------
// ProvenancePlanningQuery
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvenancePlanningQuery for SurrealProvenanceStore {
    async fn query_current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>> {
        let intents = self.query_intent_history(task_id, Some(500)).await?;
        if intents.is_empty() {
            return Ok(None);
        }
        // Find intents that are superseded (have outgoing WAS_REPLACED_BY or WAS_REFINED_BY)
        let replaced_sources = self.collect_superseded_event_ids(task_id, "Intent").await?;
        Ok(intents
            .into_iter()
            .find(|intent| !replaced_sources.contains(intent.event_id.as_str())))
    }

    async fn query_current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>> {
        let plans = self.query_plan_history(task_id, Some(500)).await?;
        if plans.is_empty() {
            return Ok(None);
        }
        let replaced_sources = self.collect_superseded_event_ids(task_id, "Plan").await?;
        Ok(plans
            .into_iter()
            .find(|plan| !replaced_sources.contains(plan.event_id.as_str())))
    }

    async fn query_intent_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningIntentRecord>> {
        let limit_val = limit.unwrap_or(100).max(1);
        let query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'Intent' AND props.a2a_task_id = $task_id"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        let (intent_incoming, intent_outgoing) =
            self.query_supersession_maps("Intent", task_id).await?;

        let mut intents = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let context_id = props.get("a2a_context_id").and_then(Value::as_str);
            let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
            let event_id = props.get("a2a_event_id").and_then(Value::as_str);
            let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
            let description = props
                .get("prov_label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (Some(context_id), Some(task_id_value), Some(event_id), Some(intent_id)) =
                (context_id, task_id_value, event_id, intent_id)
            else {
                continue;
            };
            intents.push(PlanningIntentRecord {
                context_id: ContextId::from(context_id),
                task_id: TaskId::from_external(ExternalId::new(task_id_value)),
                event_id: EventId::from(event_id),
                intent_id: intent_id.to_string(),
                description: description.to_string(),
                supersession_from_previous: intent_incoming.get(event_id).copied(),
                superseded_by_next: intent_outgoing.get(event_id).copied(),
            });
        }
        intents.sort_by_key(|r| std::cmp::Reverse(event_order_key(&r.event_id)));
        if intents.len() > limit_val {
            intents.truncate(limit_val);
        }
        Ok(intents)
    }

    async fn query_plan_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningPlanRecord>> {
        let limit_val = limit.unwrap_or(100).max(1);
        let query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'Plan' AND props.a2a_task_id = $task_id"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        let (plan_incoming, plan_outgoing) = self.query_supersession_maps("Plan", task_id).await?;

        let mut plans = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let context_id = props.get("a2a_context_id").and_then(Value::as_str);
            let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
            let event_id = props.get("a2a_event_id").and_then(Value::as_str);
            let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
            let plan_id = props.get("a2a_plan_id").and_then(Value::as_str);
            let (
                Some(context_id),
                Some(task_id_value),
                Some(event_id),
                Some(intent_id),
                Some(plan_id),
            ) = (context_id, task_id_value, event_id, intent_id, plan_id)
            else {
                continue;
            };
            let steps = self.query_plan_steps(task_id, plan_id).await?;
            plans.push(PlanningPlanRecord {
                context_id: ContextId::from(context_id),
                task_id: TaskId::from_external(ExternalId::new(task_id_value)),
                event_id: EventId::from(event_id),
                intent_id: intent_id.to_string(),
                plan_id: plan_id.to_string(),
                steps,
                supersession_from_previous: plan_incoming.get(event_id).copied(),
                superseded_by_next: plan_outgoing.get(event_id).copied(),
            });
        }
        plans.sort_by_key(|r| std::cmp::Reverse(event_order_key(&r.event_id)));
        if plans.len() > limit_val {
            plans.truncate(limit_val);
        }
        Ok(plans)
    }
}

impl SurrealProvenanceStore {
    // -----------------------------------------------------------------------
    // Graph traversal helpers
    // -----------------------------------------------------------------------

    async fn resolve_call_activity_id_for_event(&self, event_id: &str) -> Result<Option<String>> {
        let query = format!(
            "SELECT node_id FROM {TBL_NODE} WHERE (label = 'LlmCall' OR label = 'ToolCall') AND props.a2a_event_id = $event_id LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("event_id", event_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows
            .first()
            .and_then(|row| row.get("node_id").and_then(Value::as_str).map(String::from)))
    }

    async fn get_task_agent_id(&self, task_id: &TaskId) -> Result<Option<AgentId>> {
        let task_entity_id = task_entity_id_string(task_id);
        let edges = self
            .query_edges("WAS_CREATED_BY", Some(&task_entity_id), None)
            .await?;
        let Some(edge) = edges.first() else {
            return Ok(None);
        };
        let Some(te_id) = edge.get("to_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        let edges2 = self
            .query_edges("WAS_EXECUTED_BY", Some(te_id), None)
            .await?;
        let Some(edge2) = edges2.first() else {
            return Ok(None);
        };
        let Some(instance_id) = edge2.get("to_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(agent_id_str) = instance_id.strip_prefix("agent_instance:") else {
            return Ok(None);
        };
        if agent_id_str.trim().is_empty() {
            return Ok(None);
        }
        UuidId::parse_str(agent_id_str)
            .map(AgentId::from_uuid)
            .map(Some)
            .map_err(|_| ProvenanceError::InvalidEvent {
                event_id: String::new(),
                reason: format!("task agent instance id invalid UUID: {agent_id_str:?}"),
            })
    }

    async fn enforce_step_completion_gate(&self, event: &crate::events::ProvEvent) -> Result<()> {
        let ProvEventData::PlanStepStatusChanged {
            task_id,
            plan_id,
            step_id,
            new_status,
            ..
        } = event.data()
        else {
            return Ok(());
        };
        if !is_step_completed_status(new_status) {
            return Ok(());
        }
        let context_id = event.context_id().as_str().to_string();
        let deps = self
            .fetch_step_dependencies(task_id.as_str(), plan_id.as_str(), step_id.as_str())
            .await?;
        for dep in deps {
            let completed = self
                .is_step_completed(task_id.as_str(), plan_id.as_str(), &dep)
                .await?;
            if !completed {
                return Err(ProvenanceError::InvalidEvent {
                    event_id: event.id().as_str().to_string(),
                    reason: format!(
                        "step completion rejected: dependency step not completed (plan_id={plan_id}, step_id={step_id}, depends_on={dep})"
                    ),
                });
            }
        }
        let has_evidence = self
            .has_terminal_step_evidence(
                &context_id,
                task_id.as_str(),
                plan_id.as_str(),
                step_id.as_str(),
            )
            .await?;
        if !has_evidence {
            return Err(ProvenanceError::InvalidEvent {
                event_id: event.id().as_str().to_string(),
                reason: format!(
                    "step completion rejected: no terminal LLM/tool evidence linked to step (plan_id={plan_id}, step_id={step_id})"
                ),
            });
        }
        Ok(())
    }

    async fn fetch_step_dependencies(
        &self,
        task_id: &str,
        plan_id: &str,
        step_id: &str,
    ) -> Result<Vec<String>> {
        let query = format!(
            "SELECT props.a2a_depends_on AS deps FROM {TBL_NODE} WHERE label = 'PlanStep' AND props.a2a_task_id = $task_id AND props.a2a_plan_id = $plan_id AND props.a2a_step_id = $step_id LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .bind(("plan_id", plan_id.to_string()))
            .bind(("step_id", step_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(Vec::new());
        };
        let deps_raw = row.get("deps").and_then(Value::as_str).map(String::from);
        Ok(decode_depends_on(deps_raw))
    }

    async fn is_step_completed(&self, task_id: &str, plan_id: &str, step_id: &str) -> Result<bool> {
        let query = format!(
            "SELECT props.a2a_status AS status FROM {TBL_NODE} WHERE label = 'PlanStep' AND props.a2a_task_id = $task_id AND props.a2a_plan_id = $plan_id AND props.a2a_step_id = $step_id LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .bind(("plan_id", plan_id.to_string()))
            .bind(("step_id", step_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(is_step_completed_status(status))
    }

    async fn has_terminal_step_evidence(
        &self,
        context_id: &str,
        task_id: &str,
        plan_id: &str,
        step_id: &str,
    ) -> Result<bool> {
        let query = format!(
            "SELECT node_id FROM {TBL_NODE} WHERE (label = 'LlmCall' OR label = 'ToolCall') AND props.a2a_context_id = $context_id AND props.a2a_task_id = $task_id AND props.a2a_plan_id = $plan_id AND props.a2a_step_id = $step_id AND props.a2a_activity_outcome = 'Success' LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("context_id", context_id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .bind(("plan_id", plan_id.to_string()))
            .bind(("step_id", step_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(!rows.is_empty())
    }

    // -----------------------------------------------------------------------
    // Planning query helpers
    // -----------------------------------------------------------------------

    async fn query_plan_steps(
        &self,
        task_id: &TaskId,
        plan_id: &str,
    ) -> Result<Vec<PlanningPlanStepRecord>> {
        let query = format!(
            "SELECT props, props.a2a_step_order AS step_order FROM {TBL_NODE} WHERE label = 'PlanStep' AND props.a2a_task_id = $task_id AND props.a2a_plan_id = $plan_id ORDER BY props.a2a_step_order ASC"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.as_str().to_string()))
            .bind(("plan_id", plan_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        let mut steps = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let step_id = props.get("a2a_step_id").and_then(Value::as_str);
            let description = props
                .get("prov_label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let step_order = props
                .get("a2a_step_order")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let depends_on_raw = props
                .get("a2a_depends_on")
                .and_then(Value::as_str)
                .map(String::from);
            let step_status = props
                .get("a2a_status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let Some(step_id) = step_id else {
                continue;
            };
            steps.push(PlanningPlanStepRecord {
                step_id: step_id.to_string(),
                description: description.to_string(),
                order: step_order.max(0) as u32,
                depends_on: decode_depends_on(depends_on_raw),
                status: step_status.to_string(),
            });
        }
        Ok(steps)
    }

    async fn query_supersession_maps(
        &self,
        node_label: &str,
        task_id: &TaskId,
    ) -> Result<(
        HashMap<String, PlanningSupersessionKind>,
        HashMap<String, PlanningSupersessionKind>,
    )> {
        // Query edges between nodes of this label for the given task
        let replaced_edges = self
            .query_supersession_edges(node_label, task_id, semantic_labels::WAS_REPLACED_BY)
            .await?;
        let refined_edges = self
            .query_supersession_edges(node_label, task_id, semantic_labels::WAS_REFINED_BY)
            .await?;

        let mut incoming: HashMap<String, PlanningSupersessionKind> = HashMap::new();
        let mut outgoing: HashMap<String, PlanningSupersessionKind> = HashMap::new();

        for (source_event_id, target_event_id) in &replaced_edges {
            incoming
                .entry(target_event_id.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
            outgoing
                .entry(source_event_id.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
        }
        for (source_event_id, target_event_id) in &refined_edges {
            incoming
                .entry(target_event_id.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
            outgoing
                .entry(source_event_id.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
        }

        Ok((incoming, outgoing))
    }

    async fn query_supersession_edges(
        &self,
        node_label: &str,
        _task_id: &TaskId,
        rel_type: &str,
    ) -> Result<Vec<(String, String)>> {
        // Find edges between nodes of `node_label` with matching task_id
        let query = format!(
            "SELECT from_id, to_id FROM {TBL_EDGE} WHERE rel_type = $rel_type AND from_label = $label AND to_label = $label"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("rel_type", rel_type.to_string()))
            .bind(("label", node_label.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        let mut results = Vec::new();
        for row in &rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or_default();
            // Resolve event_ids from the nodes
            let from_event = self.get_node_event_id(from_id).await?;
            let to_event = self.get_node_event_id(to_id).await?;
            if let (Some(from_event), Some(to_event)) = (from_event, to_event) {
                results.push((from_event, to_event));
            }
        }
        Ok(results)
    }

    async fn get_node_event_id(&self, node_id: &str) -> Result<Option<String>> {
        let node = self.get_node(node_id).await?;
        Ok(node.and_then(|n| {
            n.get("props")
                .and_then(|p| p.get("a2a_event_id"))
                .and_then(Value::as_str)
                .map(String::from)
        }))
    }

    async fn collect_superseded_event_ids(
        &self,
        task_id: &TaskId,
        node_label: &str,
    ) -> Result<HashSet<String>> {
        let replaced_edges = self
            .query_supersession_edges(node_label, task_id, semantic_labels::WAS_REPLACED_BY)
            .await?;
        let refined_edges = self
            .query_supersession_edges(node_label, task_id, semantic_labels::WAS_REFINED_BY)
            .await?;
        let mut superseded = HashSet::new();
        for (source_event_id, _) in replaced_edges {
            superseded.insert(source_event_id);
        }
        for (source_event_id, _) in refined_edges {
            superseded.insert(source_event_id);
        }
        Ok(superseded)
    }
}

// ---------------------------------------------------------------------------
// ProvenanceOpsQuery
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvenanceOpsQuery for SurrealProvenanceStore {
    async fn query_ops(
        &self,
        mut request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse> {
        let profile = request
            .response_profile
            .clone()
            .unwrap_or(ProvenanceResponseProfile::UiFull);
        let page_cap = match profile {
            ProvenanceResponseProfile::UiFull => 200_u32,
            ProvenanceResponseProfile::ToolCompact => 50_u32,
        };
        let requested_page = request.page_size.unwrap_or(50).max(1);
        let page_size = requested_page.min(page_cap) as usize;
        let offset = request
            .cursor
            .as_deref()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        request.page_size = Some(page_size as u32);

        let label = match request.resource {
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => "LlmCall",
            ProvenanceOpsResource::ToolCalls => "ToolCall",
            ProvenanceOpsResource::Messages => "Message",
        };

        // Build filter conditions
        let mut where_clauses = vec![format!("label = '{label}'")];
        if let Some(ref ctx) = request.filters.context_id {
            where_clauses.push(format!("props.a2a_context_id = '{}'", ctx.as_str()));
        }
        if let Some(ref tid) = request.filters.task_id {
            where_clauses.push(format!("props.a2a_task_id = '{}'", tid.as_str()));
        }
        if let Some(ref tool_name) = request.filters.tool_name {
            where_clauses.push(format!("props.a2a_tool_name = '{tool_name}'"));
        }
        if let Some(ref model) = request.filters.model {
            where_clauses.push(format!("props.a2a_model = '{model}'"));
        }

        let where_clause = where_clauses.join(" AND ");
        let query = format!("SELECT * OMIT id FROM {TBL_NODE} WHERE {where_clause}");
        let mut response = self.db.query(&query).await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;

        // Convert to ops row format
        let mut ops_rows: Vec<Map<String, Value>> = rows
            .iter()
            .filter_map(|row| {
                let props = row.get("props")?.as_object()?;
                let mut out = Map::new();
                let node_id = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                out.insert(
                    "activity_id".to_string(),
                    Value::String(node_id.to_string()),
                );
                for (k, v) in props {
                    out.insert(k.clone(), v.clone());
                }
                Some(out)
            })
            .collect();

        // Sort
        let sort_by = request.sort_by.as_deref().unwrap_or("a2a_event_id");
        let sort_desc = request
            .sort_dir
            .as_deref()
            .map(|d| d.eq_ignore_ascii_case("desc"))
            .unwrap_or(true);
        ops_rows.sort_by(|a, b| {
            let av = a.get(sort_by).cloned().unwrap_or(Value::Null);
            let bv = b.get(sort_by).cloned().unwrap_or(Value::Null);
            let ord = match (av, bv) {
                (Value::Number(an), Value::Number(bn)) => an
                    .as_f64()
                    .partial_cmp(&bn.as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Value::String(as_), Value::String(bs_)) => as_.cmp(&bs_),
                _ => std::cmp::Ordering::Equal,
            };
            if sort_desc { ord.reverse() } else { ord }
        });

        let total_rows = ops_rows.len();
        let page_end = std::cmp::min(offset.saturating_add(page_size), total_rows);
        let page_rows = if offset < total_rows {
            ops_rows[offset..page_end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if page_end < total_rows {
            Some(page_end.to_string())
        } else {
            None
        };

        let _top_k = request.top_k.unwrap_or(10) as usize;
        let failed_count = ops_rows
            .iter()
            .filter(|r| {
                r.get("a2a_activity_outcome")
                    .and_then(Value::as_str)
                    .map(|s| s == "Failed")
                    .unwrap_or(false)
            })
            .count();

        let summary = serde_json::json!({
            "count": total_rows,
            "failedCount": failed_count,
        });

        Ok(ProvenanceOpsQueryResponse {
            resource: request.resource,
            rows: page_rows.into_iter().map(Value::Object).collect(),
            summary,
            hotspot_groups: Vec::new(),
            next_cursor,
            truncated: total_rows > page_size || requested_page > page_cap,
            applied_caps: Map::from_iter([
                (
                    "page_size".to_string(),
                    Value::Number((page_size as u64).into()),
                ),
                (
                    "max_page_size".to_string(),
                    Value::Number((page_cap as u64).into()),
                ),
                (
                    "top_k".to_string(),
                    Value::Number((request.top_k.unwrap_or(10) as u64).into()),
                ),
            ]),
        })
    }

    async fn resolve_archive_ref(
        &self,
        archive_ref: &str,
    ) -> Result<Option<ProvenanceArchiveRecord>> {
        let Some(parsed) = parse_archive_ref(archive_ref) else {
            return Ok(None);
        };
        match parsed {
            ParsedArchiveRef::PayloadId(payload_id) => {
                let Some(payload) = self.read_payload_by_id(payload_id).await? else {
                    return Ok(None);
                };
                Ok(Some(ProvenanceArchiveRecord {
                    archive_ref: ArchiveRef(archive_ref.to_string()),
                    payloads: vec![archive_payload_from_record(payload)?],
                }))
            }
            ParsedArchiveRef::ActivityId(activity_id) => {
                let payloads = self.read_payloads_by_activity(activity_id).await?;
                if payloads.is_empty() {
                    return Ok(None);
                }
                Ok(Some(ProvenanceArchiveRecord {
                    archive_ref: ArchiveRef(archive_ref.to_string()),
                    payloads: payloads
                        .into_iter()
                        .map(archive_payload_from_record)
                        .collect::<Result<Vec<_>>>()?,
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A2aGraphStore
// ---------------------------------------------------------------------------

#[async_trait]
impl A2aGraphStore for SurrealProvenanceStore {
    async fn max_task_ord(&self) -> A2aGraphStoreResult<i64> {
        let query = format!("SELECT ord FROM {TBL_A2A_TASK} ORDER BY ord DESC LIMIT 1");
        let mut response = self.db.query(&query).await.map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .first()
            .and_then(|r| r.get("ord").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn max_message_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        let query = format!(
            "SELECT seq FROM {TBL_A2A_MESSAGE} WHERE task_id = $task_id ORDER BY seq DESC LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .first()
            .and_then(|r| r.get("seq").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn max_update_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        let query = format!(
            "SELECT seq FROM {TBL_A2A_UPDATE} WHERE task_id = $task_id ORDER BY seq DESC LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .first()
            .and_then(|r| r.get("seq").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn get_task_node(&self, id: &str) -> A2aGraphStoreResult<Option<TaskSubgraphNode>> {
        let query =
            format!("SELECT * OMIT id FROM {TBL_A2A_TASK} WHERE task_id = $task_id LIMIT 1");
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows.first().and_then(|row| {
            Some(TaskSubgraphNode {
                id: row.get("task_id")?.as_str()?.to_string(),
                context_id: row
                    .get("context_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status_json: row
                    .get("status_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                metadata_json: row
                    .get("metadata_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                extra_json: row
                    .get("extra_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                artifacts_json: row
                    .get("artifacts_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        }))
    }

    async fn list_task_nodes(
        &self,
        context_id: Option<&str>,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphNode>> {
        let (query, _needs_bind) = if context_id.is_some() {
            (
                format!(
                    "SELECT * OMIT id FROM {TBL_A2A_TASK} WHERE context_id = $context_id ORDER BY ord"
                ),
                true,
            )
        } else {
            (
                format!("SELECT * OMIT id FROM {TBL_A2A_TASK} ORDER BY ord"),
                false,
            )
        };
        let mut q = self.db.query(&query);
        if let Some(cid) = context_id {
            q = q.bind(("context_id", cid.to_string()));
        }
        let mut response = q.await.map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TaskSubgraphNode {
                    id: row.get("task_id")?.as_str()?.to_string(),
                    context_id: row
                        .get("context_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    status_json: row
                        .get("status_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    metadata_json: row
                        .get("metadata_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    extra_json: row
                        .get("extra_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    artifacts_json: row
                        .get("artifacts_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect())
    }

    async fn upsert_task_node(
        &self,
        node: &TaskSubgraphNode,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPSERT {TBL_A2A_TASK} SET task_id = $task_id, context_id = $context_id, status_json = $status_json, metadata_json = $metadata_json, extra_json = $extra_json, artifacts_json = $artifacts_json, ord = $ord WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", node.id.clone()))
            .bind(("context_id", node.context_id.clone()))
            .bind(("status_json", node.status_json.clone()))
            .bind(("metadata_json", node.metadata_json.clone()))
            .bind(("extra_json", node.extra_json.clone()))
            .bind(("artifacts_json", node.artifacts_json.clone()))
            .bind(("ord", ord_if_create))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn ensure_task_node(
        &self,
        id: &str,
        context_id: &str,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        // Only create if not exists
        let existing = self.get_task_node(id).await?;
        if existing.is_some() {
            return Ok(());
        }
        let query = format!(
            "INSERT INTO {TBL_A2A_TASK} (task_id, context_id, status_json, metadata_json, extra_json, artifacts_json, ord) VALUES ($task_id, $context_id, '', '{{}}', '{{}}', '[]', $ord)"
        );
        self.db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .bind(("context_id", context_id.to_string()))
            .bind(("ord", ord_if_create))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn insert_message_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        message_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPSERT {TBL_A2A_MESSAGE} SET msg_id = $msg_id, task_id = $task_id, seq = $seq, message_json = $message_json WHERE msg_id = $msg_id"
        );
        self.db
            .query(&query)
            .bind(("msg_id", id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .bind(("seq", seq))
            .bind(("message_json", message_json.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn list_message_json(&self, task_id: &str) -> A2aGraphStoreResult<Vec<String>> {
        let query = format!(
            "SELECT message_json, seq FROM {TBL_A2A_MESSAGE} WHERE task_id = $task_id ORDER BY seq"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                row.get("message_json")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect())
    }

    async fn set_task_status_json(&self, id: &str, status_json: &str) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPDATE {TBL_A2A_TASK} SET status_json = $status_json WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .bind(("status_json", status_json.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn insert_update_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        kind: &str,
        payload_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPSERT {TBL_A2A_UPDATE} SET update_id = $update_id, task_id = $task_id, seq = $seq, kind = $kind, payload_json = $payload_json WHERE update_id = $update_id"
        );
        self.db
            .query(&query)
            .bind(("update_id", id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .bind(("seq", seq))
            .bind(("kind", kind.to_string()))
            .bind(("payload_json", payload_json.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn list_update_nodes(
        &self,
        task_id: &str,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphUpdateNode>> {
        let query = format!(
            "SELECT update_id, kind, payload_json, seq FROM {TBL_A2A_UPDATE} WHERE task_id = $task_id ORDER BY seq"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = response.take(0).map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TaskSubgraphUpdateNode {
                    id: row.get("update_id")?.as_str()?.to_string(),
                    kind: row.get("kind")?.as_str()?.to_string(),
                    payload_json: row.get("payload_json")?.as_str()?.to_string(),
                })
            })
            .collect())
    }

    async fn delete_update_node(&self, id: &str) -> A2aGraphStoreResult<()> {
        let query = format!("DELETE FROM {TBL_A2A_UPDATE} WHERE update_id = $update_id");
        self.db
            .query(&query)
            .bind(("update_id", id.to_string()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
