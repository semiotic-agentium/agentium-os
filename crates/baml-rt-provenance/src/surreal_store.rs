//! SurrealDB-backed provenance store — the sole provenance persistence engine.
//!
//! Implements:
//! - [`ProvenanceWriter`] + [`ProvenanceContextReader`]
//! - [`ProvenanceQueryApi`]
//! - [`A2aGraphStore`]
//! - [`ProvenancePlanningQuery`]
//! - [`ProvenanceOpsQuery`]
//!
//! ## Concurrency model
//!
//! SurrealDB is async-first with native MVCC. No global mutex or dedicated worker thread
//! is needed (unlike synchronous embedded stores that require a serialized worker for
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
//! | `provenance_payload` | Payload pointers + `search_text` for BM25 |
//! | `provenance_payload_blob` | Content-addressed JSON bodies (large tool/LLM results) |
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
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_vocabulary::{
    A2aGraphStore, A2aGraphStoreError, A2aGraphStoreResult, TaskSubgraphNode,
    TaskSubgraphUpdateNode,
};
use serde_json::{Map, Value};
use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};
use tokio::sync::Mutex;

use crate::{
    error::{ProvenanceError, Result},
    events::ProvEventData,
    mermaid_cache::MermaidCache,
    normalizer::{
        DefaultProvNormalizer, NormalizeContext, ProvNormalizer, task_entity_id_string,
        validate_event,
    },
    payload_record::{PayloadRecord, StorageKind},
    payload_storage,
    store::{
        ActivityRef, ArchiveRef, ConversationItemContent, PayloadRef, PlanningIntentRecord,
        PlanningPlanRecord, PlanningPlanStepRecord, ProvenanceArchivePayload,
        ProvenanceArchiveRecord, ProvenanceContextMessage, ProvenanceContextReader,
        ProvenanceConversationContextItem, ProvenanceOpsQuery, ProvenanceOpsQueryRequest,
        ProvenanceOpsQueryResponse, ProvenanceOpsResource, ProvenancePlanningQuery,
        ProvenanceQueryApi, ProvenanceResponseProfile, ProvenanceWriter, SessionStepContent,
        SessionStepOp, ToolCallContent, ToolOutcome, ToolResultContent, ToolSessionPhase,
    },
    surreal_tables::{
        FTS_PAYLOAD_ACTIVITY_WHERE, PAYLOAD_ROW_SELECT, TBL_A2A_MESSAGE, TBL_A2A_TASK,
        TBL_A2A_UPDATE, TBL_EDGE, TBL_NODE, TBL_PAYLOAD, TBL_PAYLOAD_BLOB,
    },
    surreal_write_batch::call_activity_id_from_normalized,
    vocabulary::semantic_labels,
};

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

const NS: &str = "provenance";
const DB: &str = "store";

// ---------------------------------------------------------------------------
// Backend enum + builder
// ---------------------------------------------------------------------------

/// Backend strategy for SurrealDB provenance store.
///
/// Storage backend strategy: file-backed (SurrealKV), in-memory shared, or in-memory isolated.
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
            activity_anchor: String::new(),
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
        // Indexes for common node property queries (context_id, task_id, activity_anchor)
        format!("DEFINE INDEX IF NOT EXISTS idx_node_context ON {TBL_NODE} FIELDS props.a2a_context_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_task ON {TBL_NODE} FIELDS props.a2a_task_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_activity_anchor ON {TBL_NODE} FIELDS props.a2a_activity_anchor"),
        // Edge table: indexed by from/to and rel_type
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_from ON {TBL_EDGE} FIELDS from_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to ON {TBL_EDGE} FIELDS to_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_rel ON {TBL_EDGE} FIELDS rel_type"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_composite ON {TBL_EDGE} FIELDS from_id, rel_type, to_id UNIQUE"),
        // Payload table: unique payload_id, indexed by activity_anchor_id and activity_id
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_id ON {TBL_PAYLOAD} FIELDS payload_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity_anchor ON {TBL_PAYLOAD} FIELDS activity_anchor_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity ON {TBL_PAYLOAD} FIELDS activity_id, payload_kind"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_blob_hash ON {TBL_PAYLOAD_BLOB} FIELDS content_hash UNIQUE"),
        // A2A task table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_id ON {TBL_A2A_TASK} FIELDS task_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_ctx ON {TBL_A2A_TASK} FIELDS context_id"),
        // A2A message table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_id ON {TBL_A2A_MESSAGE} FIELDS msg_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_task ON {TBL_A2A_MESSAGE} FIELDS task_id, seq"),
        // A2A update table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_id ON {TBL_A2A_UPDATE} FIELDS update_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_task ON {TBL_A2A_UPDATE} FIELDS task_id, seq"),
        // Full-text search on denormalized search_text (blob-backed bodies are not indexed in-table)
        "DEFINE ANALYZER IF NOT EXISTS payload_analyzer TOKENIZERS blank, class FILTERS snowball(english)".to_string(),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_search_fts ON {TBL_PAYLOAD} FIELDS search_text FULLTEXT ANALYZER payload_analyzer BM25"),
    ];
    for query in &schema_queries {
        db.query(query).await.map_err(map_surreal_error)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core store struct
// ---------------------------------------------------------------------------

/// SurrealDB-backed provenance store — the canonical implementation of all provenance traits.
pub struct SurrealProvenanceStore {
    db: Surreal<Db>,
    normalizer: Arc<dyn ProvNormalizer>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

impl SurrealProvenanceStore {
    /// Access the underlying SurrealDB connection for direct queries (graph export, tool index).
    pub fn db(&self) -> &Surreal<Db> {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_surreal_error(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

/// Deserialize SurrealDB [`surrealdb::IndexedResults`] statement `0` as JSON object rows.
#[inline]
fn query_take_zero<E>(
    response: &mut surrealdb::IndexedResults,
    map_err: impl FnOnce(surrealdb::Error) -> E,
) -> std::result::Result<Vec<Value>, E> {
    response.take(0).map_err(map_err)
}

fn payload_id_for(anchor: &str, payload_kind: &str) -> String {
    format!("payload:{anchor}:{payload_kind}")
}

fn archive_ref_for_payload(payload_id: &str) -> String {
    format!("prov:v1:payload:{payload_id}")
}

fn archive_ref_for_activity(activity_id: &str) -> String {
    format!("prov:v1:activity:{activity_id}")
}

fn activity_anchor_to_timestamp_ms(anchor: &str) -> u64 {
    anchor
        .strip_prefix("prov-")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

fn activity_anchor_order_key(anchor: &ActivityAnchorId) -> u128 {
    let digits: String = anchor
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

// ---------------------------------------------------------------------------
// Payload extraction from events
// ---------------------------------------------------------------------------

fn merge_result_error_metadata(result: Option<Value>, error: Option<Value>) -> Value {
    match (result, error) {
        (Some(result), Some(error)) => serde_json::json!({ "result": result, "error": error }),
        (Some(result), None) => result,
        (None, Some(error)) => serde_json::json!({ "error": error }),
        (None, None) => Value::Null,
    }
}

fn payload_records_from_event(event: &crate::events::ProvEvent) -> Result<Vec<PayloadRecord>> {
    let activity_anchor_id = event.id().as_str().to_string();
    match event.data() {
        ProvEventData::LlmCallStarted { prompt, .. } => {
            let payload_json =
                serde_json::to_string(prompt).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_call prompt: {e}"),
                })?;
            let search_text = payload_storage::search_text_snippet(&payload_json);
            Ok(vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_call"),
                activity_anchor_id,
                activity_id: None,
                payload_kind: "llm_call".to_string(),
                payload_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text,
            }])
        }
        ProvEventData::LlmCallCompleted {
            prompt, metadata, ..
        } => {
            let llm_call_json =
                serde_json::to_string(prompt).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_call: {e}"),
                })?;
            let llm_call_st = payload_storage::search_text_snippet(&llm_call_json);
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_call"),
                activity_anchor_id: activity_anchor_id.clone(),
                activity_id: None,
                payload_kind: "llm_call".to_string(),
                payload_json: llm_call_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: llm_call_st,
            }];
            let payload = merge_result_error_metadata(
                metadata.get("result").cloned(),
                metadata.get("error").cloned(),
            );
            let lr_json =
                serde_json::to_string(&payload).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_result: {e}"),
                })?;
            let lr_st = payload_storage::search_text_snippet(&lr_json);
            out.push(PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_result"),
                activity_anchor_id,
                activity_id: None,
                payload_kind: "llm_result".to_string(),
                payload_json: lr_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: lr_st,
            });
            Ok(out)
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
            let tc_json =
                serde_json::to_string(&tool_call).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize tool_call: {e}"),
                })?;
            let tc_st = payload_storage::search_text_snippet(&tc_json);
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "tool_call"),
                activity_anchor_id: activity_anchor_id.clone(),
                activity_id: None,
                payload_kind: "tool_call".to_string(),
                payload_json: tc_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: tc_st,
            }];
            if matches!(event.data(), ProvEventData::ToolCallCompleted { .. }) {
                let payload = merge_result_error_metadata(
                    metadata.get("result").cloned(),
                    metadata.get("error").cloned(),
                );
                let tr_json =
                    serde_json::to_string(&payload).map_err(|e| ProvenanceError::InvalidEvent {
                        activity_anchor: activity_anchor_id.clone(),
                        reason: format!("serialize tool_result: {e}"),
                    })?;
                let tr_st = payload_storage::search_text_snippet(&tr_json);
                out.push(PayloadRecord {
                    payload_id: payload_id_for(&activity_anchor_id, "tool_result"),
                    activity_anchor_id,
                    activity_id: None,
                    payload_kind: "tool_result".to_string(),
                    payload_json: tr_json,
                    content_hash: None,
                    storage_kind: StorageKind::Inline,
                    file_key: None,
                    search_text: tr_st,
                });
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn archive_payload_from_record(payload: PayloadRecord) -> Result<ProvenanceArchivePayload> {
    let payload_ref = PayloadRef(archive_ref_for_payload(&payload.payload_id));
    let activity_id = payload
        .activity_id
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: payload.activity_anchor_id.clone(),
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
            activity_anchor: payload.activity_anchor_id.clone(),
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

fn decode_payload_row(v: Value) -> Result<PayloadRecord> {
    serde_json::from_value(v).map_err(|e| ProvenanceError::CorruptPayloadRow {
        reason: e.to_string(),
    })
}

// ---------------------------------------------------------------------------
// NormalizedProv → SurrealDB write
// ---------------------------------------------------------------------------

impl SurrealProvenanceStore {
    /// Run one SurrealQL statement with no binds; return statement `0` as JSON rows.
    async fn query_sql_rows_mapped<E>(
        &self,
        sql: &str,
        map_err: impl Fn(surrealdb::Error) -> E + Copy,
    ) -> std::result::Result<Vec<Value>, E> {
        let mut response = self.db.query(sql).await.map_err(map_err)?;
        query_take_zero(&mut response, map_err)
    }

    async fn query_sql_rows(&self, sql: &str) -> Result<Vec<Value>> {
        self.query_sql_rows_mapped(sql, map_surreal_error).await
    }

    async fn run_event_write_plan(
        &self,
        plan: impl crate::surreal_write_batch::ExecutableSurrealPlan,
    ) -> Result<()> {
        let (sql, binds) = plan.into_sql_and_binds();
        if sql.trim().is_empty() {
            return Ok(());
        }
        let mut q = self.db.query(&sql);
        for crate::surreal_write_batch::TxBind { name, value } in binds {
            q = q.bind((name, value));
        }
        q.await.map_err(map_surreal_error)?;
        Ok(())
    }

    async fn read_payload_blob_body(&self, content_hash: &str) -> Result<Option<String>> {
        let query = format!("SELECT body FROM {TBL_PAYLOAD_BLOB} WHERE content_hash = $h LIMIT 1");
        let mut response = self
            .db
            .query(&query)
            .bind(("h", content_hash.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        Ok(rows.into_iter().next().and_then(|row| {
            row.get("body")
                .and_then(Value::as_str)
                .map(std::string::ToString::to_string)
        }))
    }

    async fn hydrate_payload_record(&self, mut p: PayloadRecord) -> Result<PayloadRecord> {
        if let Some(ref h) = p.content_hash
            && !h.is_empty()
            && p.payload_json.is_empty()
            && let Some(body) = self.read_payload_blob_body(h).await?
        {
            p.payload_json = body;
        }
        Ok(p)
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Payload operations
    // -----------------------------------------------------------------------

    async fn read_payload_by_id(&self, payload_id: &str) -> Result<Option<PayloadRecord>> {
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id = $payload_id LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("payload_id", payload_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        let Some(v) = rows.into_iter().next() else {
            return Ok(None);
        };
        let rec = decode_payload_row(v)?;
        Ok(Some(self.hydrate_payload_record(rec).await?))
    }

    async fn read_payload_by_activity_anchor_kind(
        &self,
        activity_anchor: &str,
        payload_kind: &str,
    ) -> Result<Option<PayloadRecord>> {
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE activity_anchor_id = $activity_anchor_id AND payload_kind = $payload_kind LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("activity_anchor_id", activity_anchor.to_string()))
            .bind(("payload_kind", payload_kind.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        let Some(v) = rows.into_iter().next() else {
            return Ok(None);
        };
        let rec = decode_payload_row(v)?;
        Ok(Some(self.hydrate_payload_record(rec).await?))
    }

    async fn read_payloads_by_activity(&self, activity_id: &str) -> Result<Vec<PayloadRecord>> {
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE activity_id = $activity_id ORDER BY payload_kind"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("activity_id", activity_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        let mut out = Vec::new();
        for v in rows {
            let rec = decode_payload_row(v)?;
            out.push(self.hydrate_payload_record(rec).await?);
        }
        Ok(out)
    }

    /// Payload text search via SurrealDB BM25 full-text index.
    /// Used by `query_ops` to filter rows by payload content.
    async fn search_payload_activity_ids(&self, query_text: &str) -> Result<Vec<String>> {
        // Normalize query text for SurrealDB full-text search.
        let normalized = normalize_payload_text_query(query_text);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }

        let query = format!(
            "SELECT DISTINCT activity_id FROM {TBL_PAYLOAD} WHERE {FTS_PAYLOAD_ACTIVITY_WHERE}"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("query_text", normalized))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

#[async_trait]
impl ProvenanceWriter for SurrealProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        validate_event(&event)?;
        self.enforce_step_completion_gate(&event).await?;
        let mut payload_records = payload_records_from_event(&event)?;
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
        let anchor = event.id().as_str().to_string();
        let activity_id = call_activity_id_from_normalized(&normalized, &anchor);

        let mut blob_bodies: Vec<(String, String)> = Vec::new();
        let mut inline_payload_bytes: usize = 0;
        for p in &mut payload_records {
            if let Some(ref a) = activity_id {
                p.activity_id = Some(a.clone());
            }
            if payload_storage::should_offload_payload(&p.payload_kind, p.payload_json.len()) {
                let v: Value = serde_json::from_str(&p.payload_json).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("payload json for offload: {e}"),
                    }
                })?;
                let canon = payload_storage::canonical_json_string(&v).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("canonical json for offload: {e}"),
                    }
                })?;
                let hash = payload_storage::sha256_hex_utf8(&canon);
                p.search_text = payload_storage::search_text_snippet(&canon);
                p.content_hash = Some(hash.clone());
                p.storage_kind = StorageKind::Blob;
                p.file_key = Some(payload_storage::logical_file_key_for_tool_archive(&hash));
                p.payload_json.clear();
                blob_bodies.push((hash, canon));
            } else {
                inline_payload_bytes = inline_payload_bytes.saturating_add(p.payload_json.len());
                p.search_text = payload_storage::search_text_snippet(&p.payload_json);
            }
        }

        let plans = crate::surreal_write_batch::build_event_write_plans(
            &normalized,
            context_id_opt.as_deref(),
            &payload_records,
            &blob_bodies,
        );
        let total_stmts: usize = plans.iter().map(|p| p.statement_count).sum();
        let total_binds: usize = plans.iter().map(|p| p.binds.len()).sum();
        tracing::debug!(
            target: "baml_rt_provenance::surreal",
            anchor = %anchor,
            txn_parts = plans.len(),
            statements = total_stmts,
            bind_count = total_binds,
            payload_rows = payload_records.len(),
            blob_rows = blob_bodies.len(),
            inline_payload_bytes,
            "provenance add_event write txn"
        );
        for plan in plans {
            self.run_event_write_plan(plan).await?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let event_id = props
                .get("a2a_activity_anchor")
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
                timestamp_ms: activity_anchor_to_timestamp_ms(event_id),
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
        let msg_rows: Vec<Value> = query_take_zero(&mut msg_response, map_surreal_error)?;

        // Fetch tool call items with proper edge topology.
        // Expected pattern: ToolCall -[WAS_USED_BY]-> ToolArgs
        // Then validates contract_holds() on prov_role and prov_type.
        //
        // Step 1: Find ToolCall nodes with completed outcomes
        let tool_query = format!(
            "SELECT node_id, props FROM {TBL_NODE} WHERE label = 'ToolCall' AND props.a2a_context_id = $ctx AND props.a2a_activity_outcome IN ['Success', 'Failed']"
        );
        let mut tool_response = self
            .db
            .query(&tool_query)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let tool_rows: Vec<Value> = query_take_zero(&mut tool_response, map_surreal_error)?;

        // Step 2: Find edges from ToolCall to ToolArgs nodes
        // and collect their prov_role/prov_type for contract validation.
        // The semantic mapping writes these as WAS_USED_BY.
        let edge_query = format!(
            "SELECT from_id, to_id, props OMIT id FROM {TBL_EDGE} WHERE rel_type = '{}'",
            semantic_labels::WAS_USED_BY
        );
        let edge_rows: Vec<Value> = self.query_sql_rows(&edge_query).await?;

        // Step 3: Find ToolArgs nodes to verify target type
        let args_query = format!("SELECT node_id, props FROM {TBL_NODE} WHERE label = 'ToolArgs'");
        let args_rows: Vec<Value> = self.query_sql_rows(&args_query).await?;

        // Build set of ToolArgs node_ids
        let tool_args_node_ids: HashSet<String> = args_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(String::from))
            .collect();

        // Build map of ToolCall node_id -> (edge_role, edge_target_type) for contract validation
        // Only include edges where to_id is a ToolArgs node
        let mut tool_call_edge_info: HashMap<String, (String, String)> = HashMap::new();
        for edge in &edge_rows {
            let from_id = edge
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = edge
                .get("to_id")
                .and_then(Value::as_str)
                .unwrap_or_default();

            // Only consider edges to ToolArgs nodes
            if !tool_args_node_ids.contains(to_id) {
                continue;
            }

            let edge_props = edge.get("props").and_then(Value::as_object);
            let prov_role = edge_props
                .and_then(|p| p.get("prov_role"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let prov_type = edge_props
                .and_then(|p| p.get("prov_type"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            tool_call_edge_info.insert(from_id.to_string(), (prov_role, prov_type));
        }

        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in &msg_rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            // Gap 11: Skip rows with missing required fields
            let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };
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
                timestamp_ms: activity_anchor_to_timestamp_ms(event_id),
                activity_anchor: ActivityAnchorId::from(event_id),
                role: role.to_string(),
                content: ConversationItemContent::Message(content),
            });
        }

        for row in &tool_rows {
            let node_id = row
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };

            // Gap 11: Skip rows with missing required fields (activity_anchor, tool_name)
            let event_id_str = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };
            let tool_name = match props.get("a2a_tool_name").and_then(Value::as_str) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => continue,
            };

            // Gap 9: Validate ToolCall-ToolArgs edge topology contract
            // Requires ToolCall -[WAS_USED_BY]-> ToolArgs edge
            // and validates contract_holds() on prov_role/prov_type
            if let Some((prov_role, prov_type)) = tool_call_edge_info.get(node_id) {
                // Contract check: role must be empty or "a2a:args", type must be empty or "a2a:ToolArgs"
                let role_ok = prov_role.is_empty() || prov_role == "a2a:args";
                let type_ok = prov_type.is_empty() || prov_type == "a2a:ToolArgs";
                if !role_ok || !type_ok {
                    continue; // Contract doesn't hold, skip this row
                }
            } else {
                // No edge to ToolArgs found - skip (MATCH requires this edge)
                continue;
            }

            // Gap 10: Read metadata for fallback when payloads are absent
            let metadata: Value = props
                .get("a2a_metadata")
                .and_then(|v| match v {
                    Value::String(s) => serde_json::from_str(s).ok(),
                    Value::Object(_) => Some(v.clone()),
                    _ => None,
                })
                .unwrap_or(Value::Object(Map::new()));

            // Read args from metadata for fallback
            let metadata_args = metadata
                .get("args")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));

            let tool_call_payload = self
                .read_payload_by_activity_anchor_kind(event_id_str, "tool_call")
                .await?;
            let tool_result_payload = self
                .read_payload_by_activity_anchor_kind(event_id_str, "tool_result")
                .await?;

            // Gap 10: Use metadata as fallback when payload is absent
            let (args, phase) = if let Some(payload) = tool_call_payload {
                let parsed: Value =
                    serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                let args = parsed
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| metadata_args.clone());
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
                // Fallback to metadata
                (metadata_args, ToolSessionPhase::from_metadata(&metadata))
            };

            // Gap 10: Use metadata for result/error fallback
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
                // Fallback to metadata
                let result = metadata
                    .get("result")
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));
                let error = metadata_error(&metadata);
                (result, error)
            };

            let has_outcome = has_meaningful_result(&result) || error.is_some();
            let include_call =
                !phase.is_session_phase() && (!is_empty_object(&args) || has_outcome);

            if include_call {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: activity_anchor_to_timestamp_ms(event_id_str),
                    activity_anchor: ActivityAnchorId::from(event_id_str),
                    role: "assistant".to_string(),
                    content: ConversationItemContent::ToolCall(ToolCallContent {
                        tool_name: tool_name.clone(),
                        args,
                        fsm_phase: phase.clone(),
                    }),
                });
            }

            if include_call {
                let outcome = if let Some(error) = error {
                    ToolOutcome::Error(error)
                } else if has_meaningful_result(&result) {
                    ToolOutcome::Result(result)
                } else {
                    ToolOutcome::StatusOnly
                };
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: activity_anchor_to_timestamp_ms(event_id_str),
                    activity_anchor: ActivityAnchorId::from(event_id_str),
                    role: "tool".to_string(),
                    content: ConversationItemContent::ToolResult(ToolResultContent {
                        tool_name: tool_name.clone(),
                        fsm_phase: phase,
                        outcome,
                    }),
                });
            }
        }

        // Process SessionStep nodes (Open/SendDone/Read within in-progress sessions).
        let step_query = format!(
            "SELECT props FROM {TBL_NODE} WHERE label = 'SessionStep' AND props.a2a_context_id = $ctx"
        );
        let step_rows: Vec<Value> = match self
            .db
            .query(&step_query)
            .bind(("ctx", ctx.to_string()))
            .await
        {
            Ok(mut resp) => match query_take_zero(&mut resp, map_surreal_error) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(error = %e, context_id = %ctx, "SessionStep take failed, omitting steps");
                    Vec::new()
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, context_id = %ctx, "SessionStep query failed, omitting steps");
                Vec::new()
            }
        };
        {
            for row in &step_rows {
                let props = match row.get("props") {
                    Some(p) => p,
                    None => continue,
                };
                let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                    Some(id) if !id.is_empty() => id.to_string(),
                    _ => continue,
                };
                let tool_name = props
                    .get("a2a_tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let op_kind = props
                    .get("op_kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let header = props
                    .get("header")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let archive_ref = props
                    .get("archive_ref")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let grep = props
                    .get("grep")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);

                let op = match op_kind {
                    "open" => SessionStepOp::Open,
                    "send_done" => match (archive_ref, header) {
                        (Some(r), Some(hdr)) => SessionStepOp::SendDone {
                            archive_ref: r,
                            header: hdr,
                        },
                        _ => continue,
                    },
                    "read" => match archive_ref {
                        Some(r) => SessionStepOp::Read {
                            archive_ref: r,
                            grep,
                            offset: 0,
                            limit: 200,
                        },
                        None => continue,
                    },
                    _ => continue,
                };

                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: activity_anchor_to_timestamp_ms(&event_id),
                    activity_anchor: ActivityAnchorId::from(event_id.as_str()),
                    role: "assistant".to_string(),
                    content: ConversationItemContent::SessionStep(SessionStepContent {
                        tool_name,
                        op,
                    }),
                });
            }
        }

        items.sort_by_key(|i| {
            (
                i.timestamp_ms,
                activity_anchor_to_timestamp_ms(i.activity_anchor.as_str()),
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
        let replaced_sources = self
            .collect_superseded_activity_anchors(task_id, "Intent")
            .await?;
        Ok(intents
            .into_iter()
            .find(|intent| !replaced_sources.contains(intent.activity_anchor_id.as_str())))
    }

    async fn query_current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>> {
        let plans = self.query_plan_history(task_id, Some(500)).await?;
        if plans.is_empty() {
            return Ok(None);
        }
        let replaced_sources = self
            .collect_superseded_activity_anchors(task_id, "Plan")
            .await?;
        Ok(plans
            .into_iter()
            .find(|plan| !replaced_sources.contains(plan.activity_anchor_id.as_str())))
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

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
            let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
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
                activity_anchor_id: ActivityAnchorId::from(event_id),
                intent_id: intent_id.to_string(),
                description: description.to_string(),
                supersession_from_previous: intent_incoming.get(event_id).copied(),
                superseded_by_next: intent_outgoing.get(event_id).copied(),
            });
        }
        intents
            .sort_by_key(|r| std::cmp::Reverse(activity_anchor_order_key(&r.activity_anchor_id)));
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let (plan_incoming, plan_outgoing) = self.query_supersession_maps("Plan", task_id).await?;

        let mut plans = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let context_id = props.get("a2a_context_id").and_then(Value::as_str);
            let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
            let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
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
                activity_anchor_id: ActivityAnchorId::from(event_id),
                intent_id: intent_id.to_string(),
                plan_id: plan_id.to_string(),
                steps,
                supersession_from_previous: plan_incoming.get(event_id).copied(),
                superseded_by_next: plan_outgoing.get(event_id).copied(),
            });
        }
        plans.sort_by_key(|r| std::cmp::Reverse(activity_anchor_order_key(&r.activity_anchor_id)));
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

    async fn get_task_agent_id(&self, task_id: &TaskId) -> Result<Option<AgentId>> {
        let task_entity_id = task_entity_id_string(task_id);
        let edges = self
            .query_edges(semantic_labels::WAS_CREATED_BY, Some(&task_entity_id), None)
            .await?;
        let Some(edge) = edges.first() else {
            return Ok(None);
        };
        let Some(te_id) = edge.get("to_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        let edges2 = self
            .query_edges(semantic_labels::WAS_EXECUTED_BY, Some(te_id), None)
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
            .map_err(|e| ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!("task agent instance id invalid UUID: {agent_id_str:?}: {e}"),
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
                    activity_anchor: event.id().as_str().to_string(),
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
                activity_anchor: event.id().as_str().to_string(),
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

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

        for (source_anchor, target_anchor) in &replaced_edges {
            incoming
                .entry(target_anchor.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
            outgoing
                .entry(source_anchor.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
        }
        for (source_anchor, target_anchor) in &refined_edges {
            incoming
                .entry(target_anchor.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
            outgoing
                .entry(source_anchor.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
        }

        Ok((incoming, outgoing))
    }

    async fn query_supersession_edges(
        &self,
        node_label: &str,
        task_id: &TaskId,
        rel_type: &str,
    ) -> Result<Vec<(String, String)>> {
        // Find edges between nodes of `node_label` where both endpoints are scoped to the task.
        let query = format!(
            "SELECT from_id, to_id FROM {TBL_EDGE} \
             WHERE rel_type = $rel_type AND from_label = $label AND to_label = $label \
               AND from_id IN (SELECT VALUE node_id FROM {TBL_NODE} WHERE label = $label AND props.a2a_task_id = $task_id) \
               AND to_id IN (SELECT VALUE node_id FROM {TBL_NODE} WHERE label = $label AND props.a2a_task_id = $task_id)"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("rel_type", rel_type.to_string()))
            .bind(("label", node_label.to_string()))
            .bind(("task_id", task_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut results = Vec::new();
        for row in &rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or_default();
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            // Resolve activity anchors from task-scoped nodes.
            let from_event = self.get_node(from_id).await?.and_then(|n| {
                n.get("props")
                    .and_then(|p| p.get("a2a_activity_anchor"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            let to_event = self.get_node(to_id).await?.and_then(|n| {
                n.get("props")
                    .and_then(|p| p.get("a2a_activity_anchor"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            });
            if let (Some(from_event), Some(to_event)) = (from_event, to_event) {
                results.push((from_event, to_event));
            }
        }
        Ok(results)
    }

    async fn collect_superseded_activity_anchors(
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
        for (source_anchor, _) in replaced_edges {
            superseded.insert(source_anchor);
        }
        for (source_anchor, _) in refined_edges {
            superseded.insert(source_anchor);
        }
        Ok(superseded)
    }

    // -----------------------------------------------------------------------
    // Ops query enrichment helpers
    // -----------------------------------------------------------------------

    /// Load agent identity map: agent_id -> (agent_package, agent_version).
    /// Queries AgentRuntimeInstance nodes for package/version metadata.
    async fn load_agent_identity_map(&self) -> Result<HashMap<String, (String, String)>> {
        let query = format!(
            "SELECT node_id, props.a2a_agent_type AS agent_package, props.a2a_agent_version AS agent_version \
             FROM {TBL_NODE} WHERE label = 'AgentRuntimeInstance'"
        );
        let rows: Vec<Value> = self.query_sql_rows(&query).await?;

        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in rows {
            let instance_id = row
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(agent_id) = agent_id_from_instance_id(instance_id) else {
                continue;
            };
            let agent_package =
                normalize_agent_field(row.get("agent_package").and_then(Value::as_str), "unknown");
            let agent_version =
                normalize_agent_field(row.get("agent_version").and_then(Value::as_str), "unknown");
            out.insert(agent_id, (agent_package, agent_version));
        }
        Ok(out)
    }

    /// Load failure classification map: activity_id -> (failure_class, failure_evidence).
    /// Queries FailureClassification nodes linked to LlmCall/ToolCall via WAS_USED_BY edges.
    /// Queries FailureClassification nodes linked via:
    ///   MATCH (call)-[used:WAS_USED_BY]->(fc:FailureClassification)
    ///   WHERE (call:LlmCall OR call:ToolCall)
    async fn load_failure_classification_map(&self) -> Result<HashMap<String, (String, String)>> {
        // Step 1: Get all FailureClassification node_ids with their class/evidence
        let fc_query = format!(
            "SELECT node_id, props.a2a_failure_class AS failure_class, props.a2a_failure_evidence AS failure_evidence \
             FROM {TBL_NODE} WHERE label = 'FailureClassification'"
        );
        let fc_rows: Vec<Value> = self.query_sql_rows(&fc_query).await?;

        // Build FC node_id -> (class, evidence) map
        let mut fc_map: HashMap<String, (String, String)> = HashMap::new();
        for row in fc_rows {
            let node_id = row
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if node_id.is_empty() {
                continue;
            }
            let class = normalize_agent_field(
                row.get("failure_class").and_then(Value::as_str),
                "failed_graph_incomplete",
            );
            let evidence = normalize_agent_field(
                row.get("failure_evidence").and_then(Value::as_str),
                "failed_graph_incomplete",
            );
            fc_map.insert(node_id, (class, evidence));
        }

        if fc_map.is_empty() {
            return Ok(HashMap::new());
        }

        // Step 2: Get all LlmCall and ToolCall node_ids (the only valid sources for FC edges)
        let call_query =
            format!("SELECT node_id FROM {TBL_NODE} WHERE label = 'LlmCall' OR label = 'ToolCall'");
        let call_rows: Vec<Value> = self.query_sql_rows(&call_query).await?;
        let call_ids: HashSet<String> = call_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(String::from))
            .collect();

        if call_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Step 3: Find edges from call nodes to FC nodes.
        // The semantic mapping writes these as WAS_USED_BY (from semantic_used_label).
        let edge_query = format!(
            "SELECT from_id, to_id OMIT id FROM {TBL_EDGE} WHERE rel_type = '{}'",
            semantic_labels::WAS_USED_BY
        );
        let edge_rows: Vec<Value> = self.query_sql_rows(&edge_query).await?;

        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in edge_rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let to_id = row
                .get("to_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Skip if from_id is empty/null or not a call node
            if from_id.is_empty() || from_id == "null" || !call_ids.contains(&from_id) {
                continue;
            }

            // Check if to_id is a FailureClassification node
            if let Some((class, evidence)) = fc_map.get(&to_id) {
                if let Some(existing) = out.get(&from_id) {
                    let incoming = (class.clone(), evidence.clone());
                    if existing != &incoming {
                        return Err(ProvenanceError::InvalidEvent {
                            activity_anchor: from_id,
                            reason: format!(
                                "multiple conflicting failure classifications for activity: existing=({}, {}), incoming=({}, {})",
                                existing.0, existing.1, incoming.0, incoming.1
                            ),
                        });
                    }
                } else {
                    out.insert(from_id, (class.clone(), evidence.clone()));
                }
            }
        }
        Ok(out)
    }

    /// Aggregate LLM call durations by message_id for a context.
    /// Returns message_id -> total_llm_duration_ms map.
    /// Aggregates LLM duration per message from the provenance graph.
    async fn load_llm_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        // Find A2AMessageProcessing nodes and their linked LlmCall nodes via WAS_INVOKED_BY edges
        // Then aggregate duration_ms by message_id
        let query = format!(
            "SELECT props.a2a_message_id AS message_id, props.a2a_duration_ms AS duration_ms \
             FROM {TBL_NODE} WHERE label = 'LlmCall' AND props.a2a_context_id = $context_id \
             AND props.a2a_duration_ms IS NOT NULL"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("context_id", context_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut out: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let message_id = row
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if message_id.is_empty() {
                continue;
            }
            let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
            *out.entry(message_id).or_insert(0) += duration;
        }
        Ok(out)
    }

    /// Aggregate tool call durations by message_id for a context.
    /// Returns message_id -> total_tool_duration_ms map.
    /// Aggregates tool duration per message from the provenance graph.
    async fn load_tool_duration_by_message(
        &self,
        context_id: &ContextId,
    ) -> Result<HashMap<String, u64>> {
        // Find ToolCall nodes with duration_ms for the context, aggregate by message_id
        let query = format!(
            "SELECT props.a2a_message_id AS message_id, props.a2a_duration_ms AS duration_ms \
             FROM {TBL_NODE} WHERE label = 'ToolCall' AND props.a2a_context_id = $context_id \
             AND props.a2a_duration_ms IS NOT NULL"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("context_id", context_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut out: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let message_id = row
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if message_id.is_empty() {
                continue;
            }
            let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
            *out.entry(message_id).or_insert(0) += duration;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// ProvenanceOpsQuery
// ---------------------------------------------------------------------------

fn ops_row_timestamp_ms(row: &Map<String, Value>) -> u64 {
    row.get("timestamp_ms").and_then(Value::as_u64).unwrap_or(0)
}

fn ops_row_is_failed(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Failed")
}

fn ops_row_is_success(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Success")
}

// ---------------------------------------------------------------------------
// Ops query parameter validation
// ---------------------------------------------------------------------------

/// Valid field names for sort_by and group_by parameters.
/// Must match the OpsField::parse allowlist.
fn parse_ops_field(raw: &str) -> Option<&str> {
    match raw {
        "activity_id"
        | "activity_kind"
        | "timestamp_ms"
        | "duration_ms"
        | "total_tokens"
        | "prompt_tokens"
        | "completion_tokens"
        | "cached_input_tokens"
        | "agent_id"
        | "agent_display"
        | "agent_package"
        | "agent_version"
        | "context_id"
        | "task_id"
        | "message_id"
        | "provider"
        | "model"
        | "tool_name"
        | "baml_prompt"
        | "role"
        | "activity_outcome"
        | "activity_status"
        | "failure_class"
        | "failure_evidence"
        | "total_processing_ms"
        | "llm_duration_ms_sum"
        | "tool_duration_ms_sum" => Some(raw),
        _ => None,
    }
}

/// Validate and parse sort_by parameter. Defaults to "timestamp_ms".
fn parse_ops_sort_by(raw: Option<&str>) -> Result<&str> {
    let field = raw.unwrap_or("timestamp_ms");
    parse_ops_field(field).ok_or_else(|| ProvenanceError::InvalidEvent {
        activity_anchor: "ops_query".to_string(),
        reason: format!("unsupported sort field: {field}"),
    })
}

/// Validate and parse sort_dir parameter. Returns true if descending. Defaults to desc.
fn parse_ops_sort_dir(raw: Option<&str>) -> Result<bool> {
    match raw.unwrap_or("desc") {
        "asc" | "ASC" => Ok(false),
        "desc" | "DESC" => Ok(true),
        other => Err(ProvenanceError::InvalidEvent {
            activity_anchor: "ops_query".to_string(),
            reason: format!("unsupported sort direction: {other}"),
        }),
    }
}

/// Validate and parse group_by parameter. Defaults to ["agent_id"] if empty.
fn parse_ops_group_by(raw: &[String]) -> Result<Vec<String>> {
    if raw.is_empty() {
        return Ok(vec!["agent_id".to_string()]);
    }
    raw.iter()
        .map(|field| {
            parse_ops_field(field)
                .map(|f| f.to_string())
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: "ops_query".to_string(),
                    reason: format!("unsupported group dimension: {field}"),
                })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Row enrichment helpers (finalize_call_row, apply_agent_identity_fields)
// ---------------------------------------------------------------------------

/// Extract agent_id from instance_id like "agent_instance:uuid"
fn agent_id_from_instance_id(instance_id: &str) -> Option<String> {
    let raw = instance_id.strip_prefix("agent_instance:")?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// Normalize agent field: trim, filter empty/null strings, use fallback.
fn normalize_agent_field(raw: Option<&str>, fallback: &str) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .unwrap_or(fallback)
        .to_string()
}

/// Parse a JSON-like string field from row props.
fn parse_json_field(row: &Map<String, Value>, field: &str) -> Option<Value> {
    let raw = row.get(field)?;
    match raw {
        Value::String(s) => serde_json::from_str(s).ok(),
        Value::Object(_) | Value::Array(_) => Some(raw.clone()),
        _ => None,
    }
}

/// Parse a JSON-like string into a Value (string fallback).
fn parse_json_like_string(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Normalize payload text search query for SurrealDB full-text search.
/// Tokenizes and normalizes terms for SurrealDB's BM25 full-text search.
/// SurrealDB `@@` operator implicitly ANDs space-separated terms, so we just need to:
/// - Split by whitespace
/// - Remove empty tokens
/// - Strip quotes from tokens (to avoid double-quoting)
/// - Join back with spaces
///
/// Returns empty string if no valid tokens found.
fn normalize_payload_text_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.replace('"', ""))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply agent identity fields (agent_package, agent_version, agent_display) from identity map.
fn apply_agent_identity_fields(
    row: &mut Map<String, Value>,
    identity_by_agent_id: &HashMap<String, (String, String)>,
) {
    let Some(agent_id) = row.get("agent_id").and_then(Value::as_str) else {
        return;
    };
    let Some((agent_package, agent_version)) = identity_by_agent_id.get(agent_id) else {
        return;
    };
    row.insert(
        "agent_package".to_string(),
        Value::String(agent_package.clone()),
    );
    row.insert(
        "agent_version".to_string(),
        Value::String(agent_version.clone()),
    );
    row.insert(
        "agent_display".to_string(),
        Value::String(format!("{agent_package}/{agent_version}")),
    );
}

/// Nest drift fields into a "drift" sub-object.
fn nest_llm_drift_fields(row: &mut Map<String, Value>) {
    let drift_citation = row.remove("drift_citation");
    let drift_score = row.remove("drift_score");
    let drift_severity = row.remove("drift_severity");
    let drift_mode = row.remove("drift_mode");
    let drift_warn_min_score = row.remove("drift_warn_min_score");
    let drift_block_min_score = row.remove("drift_block_min_score");
    let intent_text_preview = row.remove("intent_text_preview");
    let response_text_preview = row.remove("response_text_preview");
    let step_text_preview = row.remove("step_text_preview");

    let plan_intent = row.remove("plan_drift_intent_alignment");
    let plan_step = row.remove("plan_drift_step_alignment");
    let plan_traj = row.remove("plan_drift_trajectory");
    let plan_adherence = row.remove("plan_drift_adherence");
    let plan_severity = row.remove("plan_drift_composite_severity");

    let has_tactical = drift_score.is_some()
        || drift_severity.is_some()
        || drift_mode.is_some()
        || drift_warn_min_score.is_some()
        || drift_block_min_score.is_some()
        || intent_text_preview.is_some()
        || response_text_preview.is_some()
        || step_text_preview.is_some();

    let has_plan_drift = plan_intent.is_some()
        || plan_step.is_some()
        || plan_traj.is_some()
        || plan_adherence.is_some()
        || plan_severity.is_some();

    if !has_tactical && !has_plan_drift && drift_citation.is_none() {
        return;
    }

    let mut drift = Map::new();
    if let Some(value) = drift_score
        && !value.is_null()
    {
        drift.insert("score".to_string(), value);
    }
    if let Some(value) = drift_severity
        && !value.is_null()
    {
        drift.insert("severity".to_string(), value);
    }
    if let Some(value) = drift_mode
        && !value.is_null()
    {
        drift.insert("mode".to_string(), value);
    }
    if let Some(value) = drift_warn_min_score
        && !value.is_null()
    {
        drift.insert("warnMinScore".to_string(), value);
    }
    if let Some(value) = drift_block_min_score
        && !value.is_null()
    {
        drift.insert("blockMinScore".to_string(), value);
    }
    if let Some(value) = intent_text_preview
        && !value.is_null()
    {
        drift.insert("intentTextPreview".to_string(), value);
    }
    if let Some(value) = response_text_preview
        && !value.is_null()
    {
        drift.insert("responseTextPreview".to_string(), value);
    }
    if let Some(value) = step_text_preview
        && !value.is_null()
    {
        drift.insert("stepTextPreview".to_string(), value);
    }

    if let Some(value) = drift_citation
        && !value.is_null()
    {
        drift.insert("citation".to_string(), value);
    }

    // Nest plan drift fields into drift.plan sub-object.
    if has_plan_drift {
        let mut plan = Map::new();
        if let Some(v) = plan_intent
            && !v.is_null()
        {
            plan.insert("intentAlignment".to_string(), v);
        }
        if let Some(v) = plan_step
            && !v.is_null()
        {
            plan.insert("stepAlignment".to_string(), v);
        }
        if let Some(v) = plan_traj
            && !v.is_null()
        {
            plan.insert("trajectoryDrift".to_string(), v);
        }
        if let Some(v) = plan_adherence
            && !v.is_null()
        {
            plan.insert("planAdherenceScore".to_string(), v);
        }
        if let Some(v) = plan_severity
            && !v.is_null()
        {
            plan.insert("compositeSeverity".to_string(), v);
        }
        if !plan.is_empty() {
            drift.insert("plan".to_string(), Value::Object(plan));
        }
    }

    if !drift.is_empty() {
        row.insert("drift".to_string(), Value::Object(drift));
    }
}

fn percentile(sorted_values: &[f64], q: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_values.len() as f64 - 1.0) * q).round() as usize;
    sorted_values[idx.min(sorted_values.len() - 1)]
}

fn build_hotspot_groups(
    rows: &[Map<String, Value>],
    group_dims: &[String],
    top_k: usize,
) -> Vec<Value> {
    type HotspotAggregate = (Vec<Option<String>>, u64, u64, u64, u64);
    if rows.is_empty() {
        return Vec::new();
    }
    let mut groups: HashMap<String, HotspotAggregate> = HashMap::new();
    for row in rows {
        let group_values: Vec<Option<String>> = group_dims
            .iter()
            .map(|d| {
                row.get(d).and_then(|v| match v {
                    Value::Null => None,
                    Value::String(s) => {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    }
                    _ => Some(v.to_string()),
                })
            })
            .collect();
        let key = serde_json::to_string(&group_values).unwrap_or_default();
        let duration = row.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        let tokens = row.get("total_tokens").and_then(Value::as_u64).unwrap_or(0);
        let failed = u64::from(ops_row_is_failed(row));
        let entry = groups
            .entry(key)
            .or_insert_with(|| (group_values.clone(), 0, 0, 0, 0));
        entry.1 += 1;
        entry.2 += failed;
        entry.3 += duration;
        entry.4 += tokens;
    }

    let mut out: Vec<Value> = groups
        .into_iter()
        .map(
            |(_k, (group_values, count, failed, duration_sum, token_sum))| {
                let avg_duration = if count == 0 {
                    0.0
                } else {
                    duration_sum as f64 / count as f64
                };
                let avg_tokens = if count == 0 {
                    0.0
                } else {
                    token_sum as f64 / count as f64
                };
                let group_key = group_values
                    .iter()
                    .map(|v| v.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("|");
                serde_json::json!({
                    "groupKey": group_key,
                    "groupValues": group_values,
                    "groupDimensions": group_dims,
                    "count": count,
                    "failed": failed,
                    "failureRate": if count == 0 { 0.0 } else { failed as f64 / count as f64 },
                    "avgDurationMs": avg_duration,
                    "avgTotalTokens": avg_tokens
                })
            },
        )
        .collect();
    out.sort_by(|a, b| {
        let ad = a
            .get("avgDurationMs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let bd = b
            .get("avgDurationMs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        bd.partial_cmp(&ad).unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(top_k);
    out
}

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

        let compact_profile = matches!(profile, ProvenanceResponseProfile::ToolCompact);

        // Load enrichment maps for row post-processing.
        let identity_by_agent_id = self.load_agent_identity_map().await?;
        let failure_by_activity_id = self.load_failure_classification_map().await?;

        let label = match request.resource {
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => "LlmCall",
            ProvenanceOpsResource::ToolCalls => "ToolCall",
            ProvenanceOpsResource::Messages => "Message",
        };

        // Build WHERE clause with bind params only.
        let mut where_clauses = vec!["label = $label".to_string()];
        let mut binds: Vec<(String, Value)> =
            vec![("label".to_string(), Value::String(label.to_string()))];

        if let Some(ref ctx) = request.filters.context_id {
            where_clauses.push("props.a2a_context_id = $context_id".to_string());
            binds.push((
                "context_id".to_string(),
                Value::String(ctx.as_str().to_string()),
            ));
        }
        // Note: query_message_rows does NOT filter by task_id - only by context_id.
        // For parity, skip task_id filter for Messages resource.
        if !matches!(request.resource, ProvenanceOpsResource::Messages)
            && let Some(ref tid) = request.filters.task_id
        {
            where_clauses.push("props.a2a_task_id = $task_id".to_string());
            binds.push((
                "task_id".to_string(),
                Value::String(tid.as_str().to_string()),
            ));
        }
        if let Some(ref tool_name) = request.filters.tool_name {
            where_clauses.push("props.a2a_tool_name = $tool_name".to_string());
            binds.push((
                "tool_name".to_string(),
                Value::String(tool_name.to_string()),
            ));
        }
        if let Some(ref model) = request.filters.model {
            where_clauses.push("props.a2a_model = $model".to_string());
            binds.push(("model".to_string(), Value::String(model.to_string())));
        }
        if let Some(ref provider) = request.filters.provider {
            where_clauses.push("props.a2a_client = $provider".to_string());
            binds.push(("provider".to_string(), Value::String(provider.to_string())));
        }
        if let Some(ref agent_id) = request.filters.agent_id {
            where_clauses.push("props.a2a_agent_id = $agent_id".to_string());
            binds.push((
                "agent_id".to_string(),
                Value::String(agent_id.as_str().to_string()),
            ));
        }

        let query = format!(
            "SELECT node_id, props FROM {TBL_NODE} WHERE {}",
            where_clauses.join(" AND ")
        );
        let mut q = self.db.query(&query);
        for (k, v) in binds {
            q = q.bind((k, v));
        }
        let mut response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        // Canonicalize rows to the public ops shape.
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
                if let Some(v) = out.get("a2a_context_id").cloned() {
                    out.insert("context_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_task_id").cloned() {
                    out.insert("task_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_message_id").cloned() {
                    out.insert("message_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_agent_id").cloned() {
                    out.insert("agent_id".to_string(), v);
                }
                if let Some(v) = out.get("a2a_client").cloned() {
                    out.insert("provider".to_string(), v);
                }
                if let Some(v) = out.get("a2a_model").cloned() {
                    out.insert("model".to_string(), v);
                }
                if let Some(v) = out.get("a2a_tool_name").cloned() {
                    out.insert("tool_name".to_string(), v);
                }
                // Use a2a_prompt_name (base logical prompt) for display if available,
                // falling back to a2a_function_name (full variant) for backward compat.
                let baml_prompt_val = out
                    .get("a2a_prompt_name")
                    .or_else(|| out.get("a2a_function_name"))
                    .cloned();
                if let Some(v) = baml_prompt_val {
                    out.insert("baml_prompt".to_string(), v);
                }
                if let Some(v) = out.get("a2a_duration_ms").cloned() {
                    out.insert("duration_ms".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_prompt_tokens").cloned() {
                    out.insert("prompt_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_completion_tokens").cloned() {
                    out.insert("completion_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_total_tokens").cloned() {
                    out.insert("total_tokens".to_string(), v);
                }
                if let Some(v) = out.get("a2a_usage_cached_input_tokens").cloned() {
                    out.insert("cached_input_tokens".to_string(), v);
                }

                // Timestamp: prefer prov_endTime > prov_startTime > prov_time > activity_anchor fallback
                // (coalesce: prov_endTime > prov_startTime > 0).
                let timestamp_ms = out
                    .get("prov_endTime")
                    .and_then(Value::as_u64)
                    .or_else(|| out.get("prov_startTime").and_then(Value::as_u64))
                    .or_else(|| out.get("prov_time").and_then(Value::as_u64))
                    .unwrap_or_else(|| {
                        let event_id = out
                            .get("a2a_activity_anchor")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        activity_anchor_to_timestamp_ms(event_id)
                    });
                out.insert(
                    "timestamp_ms".to_string(),
                    Value::Number(timestamp_ms.into()),
                );

                // Outcome / status: messages are always Completed/Success (
                // query_message_rows unconditionally sets these). LLM/tool calls derive from
                // the a2a_activity_outcome property.
                let is_message = matches!(request.resource, ProvenanceOpsResource::Messages);
                let (activity_outcome, activity_status) = if is_message {
                    ("Success".to_string(), "Completed".to_string())
                } else {
                    let outcome = out
                        .get("a2a_activity_outcome")
                        .and_then(Value::as_str)
                        .unwrap_or("InProgress")
                        .to_string();
                    let status = if matches!(outcome.as_str(), "Success" | "Failed") {
                        "Completed".to_string()
                    } else {
                        "InProgress".to_string()
                    };
                    let normalized = match outcome.as_str() {
                        "Success" => "Success".to_string(),
                        "Failed" => "Failed".to_string(),
                        _ => "Indeterminate".to_string(),
                    };
                    (normalized, status)
                };
                out.insert(
                    "activity_outcome".to_string(),
                    Value::String(activity_outcome),
                );
                out.insert(
                    "activity_status".to_string(),
                    Value::String(activity_status),
                );
                out.insert(
                    "activity_kind".to_string(),
                    Value::String(match request.resource {
                        ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                            "llm_call".to_string()
                        }
                        ProvenanceOpsResource::ToolCalls => "tool_call".to_string(),
                        ProvenanceOpsResource::Messages => "message_turn".to_string(),
                    }),
                );
                Some(out)
            })
            .collect();

        // Load message duration aggregations (only for Messages resource with context_id filter).
        // Aggregate LLM/tool durations per message for the Messages resource.
        let (llm_duration_by_message, tool_duration_by_message) =
            if matches!(request.resource, ProvenanceOpsResource::Messages)
                && let Some(ref context_id) = request.filters.context_id
            {
                let llm_map = self.load_llm_duration_by_message(context_id).await?;
                let tool_map = self.load_tool_duration_by_message(context_id).await?;
                (llm_map, tool_map)
            } else {
                (HashMap::new(), HashMap::new())
            };

        // Enrich rows with additional fields.
        // This adds: activity_ref, payload refs, structured payloads, agent identity, failure fields, drift nesting.
        for row in &mut ops_rows {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Add activity_ref for all row types
            if !activity_id.is_empty() {
                row.insert(
                    "activity_ref".to_string(),
                    Value::String(archive_ref_for_activity(&activity_id)),
                );
            }

            // Apply agent identity fields (agent_package, agent_version, agent_display)
            apply_agent_identity_fields(row, &identity_by_agent_id);

            match request.resource {
                ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                    // Add LLM-specific enrichment
                    // Payload refs
                    if let Some(payload_id) =
                        row.get("a2a_llm_call_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "llm_call_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }
                    if let Some(payload_id) =
                        row.get("a2a_llm_result_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "llm_result_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }

                    // Structured llm_call (from payload or inline)
                    let llm_call = if compact_profile {
                        Value::Null
                    } else if let Some(payload_id) =
                        row.get("a2a_llm_call_payload_id").and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .or_else(|| parse_json_field(row, "a2a_prompt"))
                            .unwrap_or(Value::Null)
                    } else {
                        parse_json_field(row, "a2a_prompt").unwrap_or(Value::Null)
                    };
                    row.insert("llm_call".to_string(), llm_call);

                    // Structured llm_result (from payload or inline result/error)
                    let llm_result = if compact_profile {
                        Value::Null
                    } else if let Some(payload_id) =
                        row.get("a2a_llm_result_payload_id").and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .unwrap_or(Value::Null)
                    } else {
                        let result_value = parse_json_field(row, "a2a_result");
                        let error_value = parse_json_field(row, "a2a_error");
                        match (result_value, error_value) {
                            (Some(result), Some(error)) => serde_json::json!({
                                "result": result,
                                "error": error
                            }),
                            (Some(result), None) => result,
                            (None, Some(error)) => serde_json::json!({ "error": error }),
                            (None, None) => Value::Null,
                        }
                    };
                    row.insert("llm_result".to_string(), llm_result);

                    // Clean up raw fields
                    row.remove("a2a_result");
                    row.remove("a2a_error");
                    row.remove("a2a_llm_call_payload_id");
                    row.remove("a2a_llm_result_payload_id");

                    // Nest drift fields
                    // First, copy drift fields from a2a_ prefix to non-prefixed for nesting
                    if let Some(v) = row.get("a2a_drift_score").cloned() {
                        row.insert("drift_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_severity").cloned() {
                        row.insert("drift_severity".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_mode").cloned() {
                        row.insert("drift_mode".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_warn_min_score").cloned() {
                        row.insert("drift_warn_min_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_drift_block_min_score").cloned() {
                        row.insert("drift_block_min_score".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_intent_text_preview").cloned() {
                        row.insert("intent_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_response_text_preview").cloned() {
                        row.insert("response_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_step_text_preview").cloned() {
                        row.insert("step_text_preview".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_citation_drift").cloned() {
                        row.insert("drift_citation".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_intent_alignment").cloned() {
                        row.insert("plan_drift_intent_alignment".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_step_alignment").cloned() {
                        row.insert("plan_drift_step_alignment".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_trajectory").cloned() {
                        row.insert("plan_drift_trajectory".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_adherence").cloned() {
                        row.insert("plan_drift_adherence".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_plan_drift_composite_severity").cloned() {
                        row.insert("plan_drift_composite_severity".to_string(), v);
                    }
                    nest_llm_drift_fields(row);

                    // Add failure classification for failed calls (hard-fail if missing)
                    if ops_row_is_failed(row) {
                        let resolved =
                            failure_by_activity_id.get(&activity_id).ok_or_else(|| {
                                ProvenanceError::InvalidEvent {
                                activity_anchor: activity_id.clone(),
                                reason:
                                    "missing write-time failure classification for failed llm_call"
                                        .to_string(),
                            }
                            })?;
                        row.insert(
                            "failure_class".to_string(),
                            Value::String(resolved.0.clone()),
                        );
                        row.insert(
                            "failure_evidence".to_string(),
                            Value::String(resolved.1.clone()),
                        );
                    }
                }
                ProvenanceOpsResource::ToolCalls => {
                    // Add Tool-specific enrichment
                    // Payload refs
                    if let Some(payload_id) =
                        row.get("a2a_tool_call_payload_id").and_then(Value::as_str)
                    {
                        row.insert(
                            "tool_call_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }
                    if let Some(payload_id) = row
                        .get("a2a_tool_result_payload_id")
                        .and_then(Value::as_str)
                    {
                        row.insert(
                            "tool_result_ref".to_string(),
                            Value::String(archive_ref_for_payload(payload_id)),
                        );
                    }

                    // Structured tool_call (name, args, phase)
                    let tool_name = row
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let (tool_args, tool_phase) = if let Some(payload_id) =
                        row.get("a2a_tool_call_payload_id").and_then(Value::as_str)
                    {
                        if let Ok(Some(payload)) = self.read_payload_by_id(payload_id).await {
                            let parsed = parse_json_like_string(&payload.payload_json);
                            let args = parsed.get("args").cloned().unwrap_or(Value::Null);
                            let phase = parsed
                                .get("phase")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string);
                            (args, phase)
                        } else {
                            (
                                parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                                row.get("a2a_phase")
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|v| !v.is_empty())
                                    .map(str::to_string),
                            )
                        }
                    } else {
                        (
                            parse_json_field(row, "a2a_args").unwrap_or(Value::Null),
                            row.get("a2a_phase")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string),
                        )
                    };
                    row.insert(
                        "tool_call".to_string(),
                        serde_json::json!({
                            "name": tool_name,
                            "args": tool_args,
                            "phase": tool_phase
                        }),
                    );

                    // Structured tool_result (from payload or inline result/error)
                    let tool_result = if let Some(payload_id) = row
                        .get("a2a_tool_result_payload_id")
                        .and_then(Value::as_str)
                    {
                        self.read_payload_by_id(payload_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| parse_json_like_string(&p.payload_json))
                            .unwrap_or(Value::Null)
                    } else {
                        let result_value = parse_json_field(row, "a2a_result");
                        let error_value = parse_json_field(row, "a2a_error");
                        match (result_value, error_value) {
                            (Some(result), Some(error)) => serde_json::json!({
                                "result": result,
                                "error": error
                            }),
                            (Some(result), None) => result,
                            (None, Some(error)) => serde_json::json!({ "error": error }),
                            (None, None) => Value::Null,
                        }
                    };
                    row.insert("tool_result".to_string(), tool_result);

                    // Clean up raw fields
                    row.remove("a2a_args");
                    row.remove("a2a_phase");
                    row.remove("a2a_result");
                    row.remove("a2a_error");
                    row.remove("a2a_tool_call_payload_id");
                    row.remove("a2a_tool_result_payload_id");

                    // Add failure classification for failed calls (hard-fail if missing)
                    if ops_row_is_failed(row) {
                        let resolved =
                            failure_by_activity_id.get(&activity_id).ok_or_else(|| {
                                ProvenanceError::InvalidEvent {
                                activity_anchor: activity_id.clone(),
                                reason:
                                    "missing write-time failure classification for failed tool_call"
                                        .to_string(),
                            }
                            })?;
                        row.insert(
                            "failure_class".to_string(),
                            Value::String(resolved.0.clone()),
                        );
                        row.insert(
                            "failure_evidence".to_string(),
                            Value::String(resolved.1.clone()),
                        );
                    }
                }
                ProvenanceOpsResource::Messages => {
                    // Add Message-specific enrichment

                    // Get message_id for duration lookups
                    let message_id = row
                        .get("message_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();

                    // Add LLM/tool duration aggregates
                    let llm_sum = llm_duration_by_message
                        .get(&message_id)
                        .copied()
                        .unwrap_or(0);
                    let tool_sum = tool_duration_by_message
                        .get(&message_id)
                        .copied()
                        .unwrap_or(0);
                    row.insert(
                        "llm_duration_ms_sum".to_string(),
                        Value::Number(llm_sum.into()),
                    );
                    row.insert(
                        "tool_duration_ms_sum".to_string(),
                        Value::Number(tool_sum.into()),
                    );
                    row.insert(
                        "total_processing_ms".to_string(),
                        Value::Number((llm_sum + tool_sum).into()),
                    );
                    row.insert(
                        "duration_ms".to_string(),
                        Value::Number((llm_sum + tool_sum).into()),
                    );

                    // Parse message content and extract text
                    let message_content =
                        parse_json_field(row, "a2a_content").unwrap_or(Value::Array(vec![]));
                    let message_text = match &message_content {
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n"),
                        Value::String(s) => s.clone(),
                        _ => String::new(),
                    };
                    row.insert("message_content".to_string(), message_content);
                    if !message_text.is_empty() {
                        row.insert("message_text".to_string(), Value::String(message_text));
                    }
                    row.remove("a2a_content");

                    // Add role and direction fields
                    if let Some(v) = row.get("a2a_role").cloned() {
                        row.insert("role".to_string(), v);
                    }
                    if let Some(v) = row.get("a2a_direction").cloned() {
                        row.insert("direction".to_string(), v);
                    }
                }
            }
        }

        // Exclude non-terminal "open" phase tool rows from ToolCalls responses.
        if matches!(request.resource, ProvenanceOpsResource::ToolCalls) {
            ops_rows.retain(|row| {
                row.get("tool_call")
                    .and_then(|v| v.get("phase"))
                    .and_then(Value::as_str)
                    != Some("open")
            });
        }

        // Payload text filter: resolve matching activity_ids via FTS, then filter rows.
        // Payload text filtering applies to LlmCalls/ToolCalls/Aggregates only,
        // NOT for Messages (query_message_rows has no payload_text logic).
        // Empty/whitespace-only payload_text is treated as "no filter" (None),
        // not as "filter to empty set" which would return zero rows.
        let payload_text_activity_filter: Option<HashSet<String>> =
            if !matches!(request.resource, ProvenanceOpsResource::Messages)
                && let Some(ref payload_text) = request.filters.payload_text
            {
                // Check if normalized query would be empty - if so, treat as no filter
                let normalized = normalize_payload_text_query(payload_text);
                if normalized.is_empty() {
                    None
                } else {
                    let matching = self.search_payload_activity_ids(payload_text).await?;
                    Some(matching.into_iter().collect())
                }
            } else {
                None
            };

        // Rust-side common filters.
        let prompt_filter_lc = request
            .filters
            .baml_prompt
            .as_ref()
            .map(|prompt| prompt.to_ascii_lowercase());
        let outcome_segment = request
            .outcome
            .clone()
            .unwrap_or(crate::store::ProvenanceOutcomeSegment::Both);
        ops_rows.retain(|row| {
            if let Some(from_ms) = request.filters.from_timestamp_ms
                && ops_row_timestamp_ms(row) < from_ms
            {
                return false;
            }
            if let Some(to_ms) = request.filters.to_timestamp_ms
                && ops_row_timestamp_ms(row) > to_ms
            {
                return false;
            }
            if let Some(prompt_lc) = prompt_filter_lc.as_ref() {
                let prompt_value = row
                    .get("baml_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !prompt_value.contains(prompt_lc) {
                    return false;
                }
            }
            if let Some(ref allowed) = payload_text_activity_filter {
                let activity_id = row
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !allowed.contains(activity_id) {
                    return false;
                }
            }
            match outcome_segment {
                crate::store::ProvenanceOutcomeSegment::FailedOnly => ops_row_is_failed(row),
                crate::store::ProvenanceOutcomeSegment::SuccessfulOnly => ops_row_is_success(row),
                crate::store::ProvenanceOutcomeSegment::Both => {
                    ops_row_is_success(row) || ops_row_is_failed(row)
                }
            }
        });

        // Validate and apply sort parameters.
        let sort_by = parse_ops_sort_by(request.sort_by.as_deref())?;
        let sort_desc = parse_ops_sort_dir(request.sort_dir.as_deref())?;
        ops_rows.sort_by(|a, b| {
            let av = a.get(sort_by).cloned().unwrap_or(Value::Null);
            let bv = b.get(sort_by).cloned().unwrap_or(Value::Null);
            let ord = match (&av, &bv) {
                (Value::Number(an), Value::Number(bn)) => an
                    .as_f64()
                    .partial_cmp(&bn.as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Value::String(as_), Value::String(bs_)) => as_.cmp(bs_),
                _ => std::cmp::Ordering::Equal,
            };
            let ord = if ord == std::cmp::Ordering::Equal {
                let aid = a
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let bid = b
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                aid.cmp(bid)
            } else {
                ord
            };
            if sort_desc { ord.reverse() } else { ord }
        });

        let mut durations: Vec<f64> = ops_rows
            .iter()
            .filter_map(|r| r.get("duration_ms").and_then(Value::as_f64))
            .collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut tokens: Vec<f64> = ops_rows
            .iter()
            .filter_map(|r| r.get("total_tokens").and_then(Value::as_f64))
            .collect();
        tokens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let duration_p95 = percentile(&durations, 0.95);
        let duration_p99 = percentile(&durations, 0.99);
        let token_p95 = percentile(&tokens, 0.95);
        let token_p99 = percentile(&tokens, 0.99);

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

        let top_k = request.top_k.unwrap_or(10) as usize;
        // Validate group_by.
        let effective_group_by = parse_ops_group_by(&request.group_by)?;
        let hotspot_groups = build_hotspot_groups(&ops_rows, &effective_group_by, top_k);
        let failed_count = ops_rows.iter().filter(|r| ops_row_is_failed(r)).count();
        let total_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("total_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let prompt_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let completion_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| {
                r.get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum();
        let cached_input_tokens_sum: u64 = ops_rows
            .iter()
            .map(|r| {
                r.get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum();
        let total_duration_sum: u64 = ops_rows
            .iter()
            .map(|r| r.get("duration_ms").and_then(Value::as_u64).unwrap_or(0))
            .sum();

        let mut summary = serde_json::json!({
            "count": total_rows,
            "failedCount": failed_count,
            "durationMsTotal": total_duration_sum,
            "totalTokens": total_tokens_sum,
            "promptTokensTotal": prompt_tokens_sum,
            "completionTokensTotal": completion_tokens_sum,
            "latencyHotspots": {
                "p95": duration_p95,
                "p99": duration_p99
            },
            "tokenHotspots": {
                "p95": token_p95,
                "p99": token_p99
            }
        });
        if matches!(
            request.resource,
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates
        ) && let Some(obj) = summary.as_object_mut()
        {
            obj.insert(
                "cachedInputTokensTotal".to_string(),
                Value::from(cached_input_tokens_sum),
            );
        }

        Ok(ProvenanceOpsQueryResponse {
            resource: request.resource,
            rows: page_rows.into_iter().map(Value::Object).collect(),
            summary,
            hotspot_groups,
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
        let rows: Vec<Value> = self
            .query_sql_rows_mapped(&query, A2aGraphStoreError::backend)
            .await?;
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
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
        let mut response = q.await.map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
        // ord is only set on create (ON CREATE SET semantics).
        // On update, all other fields are overwritten but ord is preserved.
        let query = format!(
            "UPSERT {TBL_A2A_TASK} SET task_id = $task_id, context_id = $context_id, \
             status_json = $status_json, metadata_json = $metadata_json, \
             extra_json = $extra_json, artifacts_json = $artifacts_json, \
             ord = IF ord IS NONE THEN $ord ELSE ord END \
             WHERE task_id = $task_id"
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
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn ensure_task_node(
        &self,
        id: &str,
        context_id: &str,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        // Atomic: UPSERT creates if no match, does nothing meaningful on match
        // (all fields are idempotently re-set to their current values by the WHERE match).
        // On create, the defaults mirror ON CREATE SET semantics.
        let query = format!(
            "UPSERT {TBL_A2A_TASK} SET task_id = $task_id, \
             context_id = IF context_id IS NONE THEN $context_id ELSE context_id END, \
             status_json = IF status_json IS NONE THEN '' ELSE status_json END, \
             metadata_json = IF metadata_json IS NONE THEN '{{}}' ELSE metadata_json END, \
             extra_json = IF extra_json IS NONE THEN '{{}}' ELSE extra_json END, \
             artifacts_json = IF artifacts_json IS NONE THEN '[]' ELSE artifacts_json END, \
             ord = IF ord IS NONE THEN $ord ELSE ord END \
             WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .bind(("context_id", context_id.to_string()))
            .bind(("ord", ord_if_create))
            .await
            .map_err(A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
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
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }
}
