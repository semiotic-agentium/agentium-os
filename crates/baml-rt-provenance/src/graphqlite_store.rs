//! GraphQLite-backed provenance store.
//!
//! One logical graph per DB (one file path or one `:memory:` connection). The
//! GraphQLite [Graph] is the persistence interface for this module; it is not
//! `Sync`, so we use a dedicated worker thread that owns the graph and runs Cypher.
//! The store is `Send + Sync` via a channel to that
//! worker. Read path uses strong-typed row extraction via GraphQLite's `Row::get`.
//!
//! ## Concurrency caveats (GraphQLite extension, not SQLite)
//!
//! **We serialize all Cypher execution in the process.** The GraphQLite extension's
//! generated Cypher scanner (Flex) uses **process-global mutable state** (e.g.
//! `current_scanner`, `current_token`, line/column globals). Concurrent Cypher
//! parses in the same process can corrupt that state and produce parse errors or
//! crashes. SQLite is fine with multiple connections; the extension's parser is not.
//!
//! **What we do:** On the **async host** we use a process-global `tokio::sync::Mutex`
//! so only one Cypher request (read or write) is in flight at a time. The caller
//! holds the lock (await) for the duration of send + reply; the actual Cypher run
//! happens in a **dedicated worker thread** that owns the [Connection]. We do not
//! block the async runtime with Cypher execution.
//!
//! **Upstream fix required:** This should be fixed in GraphQLite by making the
//! scanner reentrant (Flex `%option reentrant` and per-call state) so that
//! multiple threads or connections can parse Cypher concurrently. Until then we
//! document the limitation and keep this single-lane design.
//!
//! **Backend.** [GraphqliteBackend] configures how stores are built:
//! - **File:** one shared store per path; [build_store](GraphqliteBackend::build_store)
//!   for the same path returns a clone. Serialized Cypher (single worker).
//! - **In-memory shared:** first build creates one connection and one worker;
//!   subsequent builds return a clone. Same serialized access.
//!
//! [Graph]: graphqlite::Graph

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::Instant,
};

use async_trait::async_trait;
use baml_rt_core::ids::{AgentId, ContextId, EventId, MessageId, TaskId, UuidId};
use graphqlite::{Connection, CypherResult, Graph, Row, Value as GraphqliteValue};
use serde_json::{Map, Value};
use tokio::sync::{Mutex as TokioMutex, oneshot};

#[allow(unused_imports)] // AgentBootedEvent used in #[cfg(test)] mod tests
use crate::{
    cypher_build::{self, KeyStyle},
    error::{ProvenanceError, Result},
    events::{AgentBootedEvent, ProvEventData},
    graph_export::activity_outcome::NodeActivityOutcome,
    graph_model::{ConversationReadModel, GraphNodeLabel, TOOL_CALL_ARGS_EDGE},
    graphqlite_config::GraphqliteStoreConfig,
    mermaid_cache::MermaidCache,
    normalizer::{
        DefaultProvNormalizer, NormalizeContext, ProvNormalizer, task_entity_id_string,
        validate_event,
    },
    spans,
    store::{
        ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
        ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse,
        ProvenanceOpsResource, ProvenanceOutcomeSegment, ProvenanceQueryApi,
        ProvenanceResponseProfile, ProvenanceWriter, ToolSessionPhase,
    },
};

// Column names from ConversationReadModel RETURN clauses (storage-safe underscore form).
const MSG_COL_EVENT_ID: &str = "m.a2a_event_id";
const MSG_COL_MESSAGE_ID: &str = "m.a2a_message_id";
const MSG_COL_DIRECTION: &str = "m.a2a_direction";
const MSG_COL_ROLE: &str = "m.a2a_role";
const MSG_COL_CONTENT: &str = "m.a2a_content";
const MSG_COL_AGENT_ID: &str = "m.a2a_agent_id";

const TOOL_COL_EVENT_ID: &str = "t.a2a_event_id";
const TOOL_COL_TOOL_NAME: &str = "t.a2a_tool_name";
const TOOL_COL_METADATA: &str = "t.a2a_metadata";
const TOOL_COL_ARGS: &str = "args.a2a_args";
const TOOL_COL_ACTIVITY_OUTCOME: &str = "t.a2a_activity_outcome";
const TOOL_COL_AGENT_ID: &str = "t.a2a_agent_id";
const TOOL_COL_ROLE: &str = "used.prov_role";
const TOOL_COL_TARGET_TYPE: &str = "args.prov_type";

const PAYLOAD_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS provenance_payload (
    payload_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL,
    activity_id TEXT,
    payload_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_provenance_payload_activity_kind
ON provenance_payload(activity_id, payload_kind);
CREATE INDEX IF NOT EXISTS idx_provenance_payload_event
ON provenance_payload(event_id);
CREATE VIRTUAL TABLE IF NOT EXISTS provenance_payload_fts USING fts5(
    payload_id UNINDEXED,
    event_id UNINDEXED,
    activity_id UNINDEXED,
    payload_kind UNINDEXED,
    payload_text
);";
const UPSERT_PAYLOAD_SQL: &str =
    "INSERT INTO provenance_payload (payload_id, event_id, activity_id, payload_kind, payload_json)
    VALUES (?1, ?2, ?3, ?4, ?5)
    ON CONFLICT(payload_id) DO UPDATE SET
        event_id = excluded.event_id,
        activity_id = excluded.activity_id,
        payload_kind = excluded.payload_kind,
        payload_json = excluded.payload_json";
const UPSERT_PAYLOAD_FTS_SQL: &str =
    "INSERT INTO provenance_payload_fts (payload_id, event_id, activity_id, payload_kind, payload_text)
    VALUES (?1, ?2, ?3, ?4, ?5)";
const DELETE_PAYLOAD_FTS_SQL: &str = "DELETE FROM provenance_payload_fts WHERE payload_id = ?1";
const SELECT_PAYLOAD_BY_ID_SQL: &str = "SELECT payload_id, event_id, activity_id, payload_kind, payload_json FROM provenance_payload WHERE payload_id = ?1";
const SELECT_PAYLOAD_BY_EVENT_KIND_SQL: &str = "SELECT payload_id, event_id, activity_id, payload_kind, payload_json FROM provenance_payload WHERE event_id = ?1 AND payload_kind = ?2 LIMIT 1";
const SELECT_PAYLOAD_ACTIVITY_IDS_BY_TEXT_SQL: &str = "SELECT DISTINCT activity_id FROM provenance_payload_fts WHERE provenance_payload_fts MATCH ?1 AND activity_id IS NOT NULL";

/// Public alias so downstream crates can avoid a direct graphqlite dependency.
pub type GraphCypherResult = CypherResult;
/// Public alias so downstream crates can decode typed rows without graphqlite in Cargo.toml.
pub type GraphRow = Row;
/// Typed parameter map for Graph query_builder execution.
pub type GraphQueryParams = Map<String, Value>;
type QueryParams = GraphQueryParams;

enum WorkerRequest {
    ReadWithParams(
        String,
        QueryParams,
        oneshot::Sender<std::result::Result<CypherResult, graphqlite::Error>>,
    ),
    Write(
        String,
        QueryParams,
        oneshot::Sender<std::result::Result<(), graphqlite::Error>>,
    ),
    ReadPayloadById(
        String,
        oneshot::Sender<std::result::Result<Option<PayloadRecord>, graphqlite::Error>>,
    ),
    ReadPayloadByEventKind(
        String,
        String,
        oneshot::Sender<std::result::Result<Option<PayloadRecord>, graphqlite::Error>>,
    ),
    SearchPayloadActivityIds(
        String,
        oneshot::Sender<std::result::Result<Vec<String>, graphqlite::Error>>,
    ),
    UpsertPayload(
        PayloadRecord,
        oneshot::Sender<std::result::Result<(), graphqlite::Error>>,
    ),
}

/// Provenance-only store backed by GraphQLite (SQLite + Cypher).
/// A worker thread owns the connection; the store is Send + Sync via channel.
pub struct GraphqliteProvenanceStore {
    request_tx: mpsc::SyncSender<WorkerRequest>,
    normalizer: Arc<dyn ProvNormalizer>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

/// Strong-typed message row from GraphQLite result. No parsing; use Row::get.
struct MessageRow {
    event_id: String,
    message_id: String,
    #[allow(dead_code)] // Part of query result; reserved for future filtering or display.
    direction: String,
    role: String,
    content: Value,
    agent_id: Option<AgentId>,
}

#[derive(Debug, Clone)]
struct PayloadRecord {
    payload_id: String,
    event_id: String,
    activity_id: Option<String>,
    payload_kind: String,
    payload_json: String,
}

fn graphqlite_value_to_json(value: &GraphqliteValue) -> Value {
    match value {
        GraphqliteValue::Null => Value::Null,
        GraphqliteValue::Bool(v) => Value::Bool(*v),
        GraphqliteValue::Integer(v) => Value::Number((*v).into()),
        GraphqliteValue::Float(v) => {
            serde_json::Number::from_f64(*v).map_or(Value::Null, Value::Number)
        }
        GraphqliteValue::String(s) => parse_json_like_string(s),
        GraphqliteValue::Array(items) => {
            Value::Array(items.iter().map(graphqlite_value_to_json).collect())
        }
        GraphqliteValue::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), graphqlite_value_to_json(v));
            }
            Value::Object(out)
        }
    }
}

fn read_json_column(row: &Row, col: &str) -> std::result::Result<Value, graphqlite::Error> {
    if let Some(value) = row.get_value(col) {
        return Ok(graphqlite_value_to_json(value));
    }
    if let Ok(raw) = row.get::<String>(col) {
        return Ok(parse_json_like_string(&raw));
    }
    Err(graphqlite::Error::ColumnNotFound(col.to_string()))
}

fn read_optional_string_column(row: &Row, col: &str) -> Option<String> {
    if let Some(value) = row.get_value(col) {
        return match graphqlite_value_to_json(value) {
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Value::Null => None,
            _ => None,
        };
    }
    row.get::<String>(col).ok().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn parse_agent_id_str(raw: &str) -> Option<AgentId> {
    UuidId::parse_str(raw).ok().map(AgentId::from_uuid)
}

fn parse_agent_id_column(row: &Row, col: &str) -> Option<AgentId> {
    read_optional_string_column(row, col).and_then(|raw| parse_agent_id_str(&raw))
}

fn parse_json_like_string(raw: &str) -> Value {
    let parsed =
        serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
    if let Value::String(inner) = parsed {
        let trimmed = inner.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return serde_json::from_str::<Value>(&inner).unwrap_or(Value::String(inner));
        }
        return Value::String(inner);
    }
    parsed
}

fn init_payload_table(graph: &Graph) -> std::result::Result<(), graphqlite::Error> {
    graph
        .connection()
        .sqlite_connection()
        .execute_batch(PAYLOAD_TABLE_SQL)?;
    Ok(())
}

fn payload_id_for(event_id: &str, payload_kind: &str) -> String {
    format!("payload:{event_id}:{payload_kind}")
}

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

impl MessageRow {
    fn from_row(row: &Row) -> std::result::Result<Self, graphqlite::Error> {
        let event_id: String = row.get(MSG_COL_EVENT_ID)?;
        let message_id: String = row.get(MSG_COL_MESSAGE_ID)?;
        let direction: String = row.get(MSG_COL_DIRECTION)?;
        let role: String = row.get(MSG_COL_ROLE)?;
        let content = read_json_column(row, MSG_COL_CONTENT)?;
        let agent_id = parse_agent_id_column(row, MSG_COL_AGENT_ID);
        Ok(Self {
            event_id,
            message_id,
            direction,
            role,
            content,
            agent_id,
        })
    }
}

/// Strong-typed tool-call row from GraphQLite result.
struct ToolCallRow {
    event_id: String,
    tool_name: String,
    metadata: Value,
    args: Value,
    agent_id: Option<AgentId>,
    role: String,
    target_type: String,
    activity_outcome: Option<NodeActivityOutcome>,
}

fn decode_activity_outcome(row: &Row, col: &str) -> Option<NodeActivityOutcome> {
    let raw: String = row.get(col).ok()?;
    match raw.trim() {
        "Success" => Some(NodeActivityOutcome::Success),
        "Failed" => Some(NodeActivityOutcome::Failed),
        "InProgress" => Some(NodeActivityOutcome::InProgress),
        _ => {
            tracing::debug!(
                column = %col,
                value = %raw,
                "unable to parse activity_outcome from string"
            );
            None
        }
    }
}

impl ToolCallRow {
    fn from_row(row: &Row) -> std::result::Result<Self, graphqlite::Error> {
        let event_id: String = row.get(TOOL_COL_EVENT_ID)?;
        let tool_name: String = row.get(TOOL_COL_TOOL_NAME)?;
        let metadata = read_json_column(row, TOOL_COL_METADATA)?;
        let args = read_json_column(row, TOOL_COL_ARGS)?;
        let agent_id = parse_agent_id_column(row, TOOL_COL_AGENT_ID).or_else(|| {
            metadata
                .get("agent_id")
                .and_then(Value::as_str)
                .and_then(parse_agent_id_str)
        });
        let role: String = row.get(TOOL_COL_ROLE).unwrap_or_default();
        let target_type: String = row.get(TOOL_COL_TARGET_TYPE).unwrap_or_default();
        let activity_outcome = decode_activity_outcome(row, TOOL_COL_ACTIVITY_OUTCOME);
        Ok(Self {
            event_id,
            tool_name,
            metadata,
            args,
            agent_id,
            role,
            target_type,
            activity_outcome,
        })
    }

    fn is_completed(&self) -> bool {
        self.activity_outcome
            .map(NodeActivityOutcome::is_completed)
            .unwrap_or(false)
    }

    fn contract_holds(&self) -> bool {
        // If explicit edge/type properties are missing, infer contract from the
        // matched topology: ToolCall -[:WAS_USED_BY]-> ToolArgs.
        let role_ok = self.role.is_empty() || self.role == TOOL_CALL_ARGS_EDGE.role_value;
        let type_ok = self.target_type.is_empty()
            || self.target_type == TOOL_CALL_ARGS_EDGE.target_type_value;
        role_ok && type_ok
    }
}

fn event_id_to_timestamp_ms(event_id: &str) -> u64 {
    event_id
        .strip_prefix("prov-")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
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

fn metadata_error(metadata: &Value) -> Option<Value> {
    let error = metadata.get("error")?;
    if has_meaningful_result(error) {
        Some(error.clone())
    } else {
        None
    }
}

fn map_graphqlite_error(e: graphqlite::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

fn run_query_builder_with_params(
    graph: &Graph,
    query: &str,
    params: &QueryParams,
) -> std::result::Result<CypherResult, graphqlite::Error> {
    let mut builder = graph.query_builder(query);
    for (key, value) in params {
        builder = builder.param(key, value.clone());
    }
    builder.run()
}

fn require_object_params(params: &Value) -> Result<QueryParams> {
    match params {
        Value::Object(map) => Ok(map.clone()),
        _ => Err(ProvenanceError::Storage(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cypher params must be an object map",
        )))),
    }
}

/// Ensure GRAPHQLITE_EXTENSION_PATH is set. When using the submodule build, the extension
/// is copied next to the binary; we set the env var so graphqlite's find_extension finds it.
pub(crate) fn ensure_extension_path() {
    if std::env::var("GRAPHQLITE_EXTENSION_PATH").is_ok() {
        return;
    }
    let ext_name = if cfg!(target_os = "macos") {
        "graphqlite.dylib"
    } else if cfg!(target_os = "windows") {
        "graphqlite.dll"
    } else {
        "graphqlite.so"
    };
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let path = dir.join(ext_name);
        if path.exists() {
            // SAFETY: Single-threaded init before any graphqlite connection; no other thread reads this env.
            unsafe { std::env::set_var("GRAPHQLITE_EXTENSION_PATH", path) };
        }
    }
}

impl GraphqliteProvenanceStore {
    /// Open a graph from config and enable WAL if requested.
    fn open_graph(config: &GraphqliteStoreConfig) -> std::result::Result<Graph, graphqlite::Error> {
        ensure_extension_path();
        let conn = match &config.path {
            crate::graphqlite_config::StorePath::InMemory => Connection::open_in_memory()?,
            crate::graphqlite_config::StorePath::File(path) => Connection::open(path.as_path())?,
        };
        if config.wal {
            conn.sqlite_connection()
                .execute_batch("PRAGMA journal_mode=WAL")
                .map_err(|e| graphqlite::Error::Cypher(e.to_string()))?;
            // In WAL mode, NORMAL skips fsync per commit while still guaranteeing
            // durability against process crashes (risk only on simultaneous power
            // failure + OS crash). Eliminates the full_fsync overhead visible in
            // flamegraph profiles.
            conn.sqlite_connection()
                .execute_batch("PRAGMA synchronous=NORMAL")
                .map_err(|e| graphqlite::Error::Cypher(e.to_string()))?;
        }
        Ok(Graph::from_connection(conn))
    }
}

/// Backend strategy: one shared store (one connection/worker) per DB path or one shared in-memory store.
#[derive(Clone, Debug)]
pub enum GraphqliteBackend {
    /// File-backed: one shared store per path; all callers for the same path get the same [GraphqliteProvenanceStore]. Serialized Cypher (single worker).
    File(GraphqliteStoreConfig),
    /// In-memory shared: first [build_store] creates one connection and one worker; subsequent calls return a clone of the same store. Serialized access (single worker).
    InMemoryShared,
}

impl GraphqliteBackend {
    /// File-backed store. One shared store per path; subsequent [build_store] for the same path return a clone.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(GraphqliteStoreConfig::file(path))
    }

    /// In-memory store shared by all callers. First build creates the store; subsequent builds return a clone. Access serialized through one worker.
    pub fn in_memory_shared() -> Self {
        Self::InMemoryShared
    }

    /// Build a store. File: one shared store per path (one connection/worker). InMemoryShared: shared store, cloned.
    /// `mermaid_cache` is applied to file-backed config for add_event invalidation.
    pub fn build_store(
        &self,
        mermaid_cache: Option<Arc<MermaidCache>>,
    ) -> Result<Arc<GraphqliteProvenanceStore>> {
        match self {
            GraphqliteBackend::File(config) => {
                let path =
                    config
                        .path
                        .file_path()
                        .ok_or_else(|| ProvenanceError::InvalidEvent {
                            event_id: String::new(),
                            reason: "file backend requires a file path".to_string(),
                        })?;
                let mut config = config.clone();
                if let Some(ref cache) = mermaid_cache {
                    config = config.with_mermaid_cache(cache.clone());
                }
                get_or_init_file_store(path, config)
            }
            GraphqliteBackend::InMemoryShared => get_or_init_shared_in_memory_store(),
        }
    }
}

/// Serializes connection open only. GraphQLite loads its extension per connection;
/// concurrent opens can race in the extension's C init.
static EXTENSION_LOAD_SERIAL: Mutex<()> = Mutex::new(());

/// Serializes Cypher requests on the async host. Only one Cypher (read or write)
/// runs at a time in the process; the caller holds this lock for send + await reply.
/// The worker thread runs Cypher without a mutex. Initialized on first use.
static CYPHER_REQUEST_SERIAL: OnceLock<TokioMutex<()>> = OnceLock::new();

/// One shared store per file path. Values are Result so we cache init failures and propagate (no unwrap).
type FileStoreEntry = std::result::Result<Arc<GraphqliteProvenanceStore>, String>;
static FILE_STORES: OnceLock<Mutex<HashMap<PathBuf, FileStoreEntry>>> = OnceLock::new();

static SHARED_IN_MEMORY_STORE: OnceLock<Arc<GraphqliteProvenanceStore>> = OnceLock::new();

fn get_or_init_file_store(
    path: PathBuf,
    config: GraphqliteStoreConfig,
) -> Result<Arc<GraphqliteProvenanceStore>> {
    let mutex = FILE_STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = mutex.lock().map_err(|e| {
        ProvenanceError::Storage(Box::new(std::io::Error::other(format!(
            "file stores mutex poisoned: {e:?}",
        ))))
    })?;
    let entry = guard
        .entry(path)
        .or_insert_with(|| build_store_from_config(&config).map_err(|e| e.to_string()));
    match entry.as_ref() {
        Ok(store) => Ok(store.clone()),
        Err(msg) => Err(ProvenanceError::Storage(Box::new(std::io::Error::other(
            msg.clone(),
        )))),
    }
}

fn get_or_init_shared_in_memory_store() -> Result<Arc<GraphqliteProvenanceStore>> {
    // OnceLock::get_or_init is infallible (closure returns T, not Result). Init failure (connection
    // or extension load) is treated as fatal: we panic so the process can be restarted. Production
    // Rust prefers no unwrap; we document why this one is acceptable (no fallible init API for OnceLock).
    let store = SHARED_IN_MEMORY_STORE.get_or_init(|| {
        build_store_from_config(&GraphqliteStoreConfig::in_memory())
            .expect("shared in-memory store init: connection or extension load failed")
    });
    Ok(store.clone())
}

fn build_store_from_config(
    config: &GraphqliteStoreConfig,
) -> Result<Arc<GraphqliteProvenanceStore>> {
    let graph = {
        let _guard = EXTENSION_LOAD_SERIAL.lock().map_err(|e| {
            ProvenanceError::Storage(Box::new(std::io::Error::other(format!(
                "extension load mutex poisoned: {e:?}",
            ))))
        })?;
        GraphqliteProvenanceStore::open_graph(config).map_err(map_graphqlite_error)?
    };
    init_payload_table(&graph).map_err(map_graphqlite_error)?;
    let (request_tx, request_rx) = mpsc::sync_channel::<WorkerRequest>(256);
    thread::spawn(move || {
        let graph = graph;
        while let Ok(req) = request_rx.recv() {
            match req {
                WorkerRequest::ReadWithParams(query, params, reply) => {
                    let span = spans::cypher_execute(&query, &Value::Object(params.clone()));
                    let _guard = span.enter();
                    tracing::debug!(query_text = %query, params = ?params, "cypher execute");
                    let result = run_query_builder_with_params(&graph, &query, &params);
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
                WorkerRequest::Write(query, params, reply) => {
                    let span = spans::cypher_execute(&query, &Value::Object(params.clone()));
                    let _guard = span.enter();
                    tracing::debug!(query_text = %query, params = ?params, "cypher execute");
                    let result = run_query_builder_with_params(&graph, &query, &params).map(|_| ());
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
                WorkerRequest::ReadPayloadById(payload_id, reply) => {
                    let row = graph.connection().sqlite_connection().query_row(
                        SELECT_PAYLOAD_BY_ID_SQL,
                        [payload_id.as_str()],
                        |row| {
                            Ok(PayloadRecord {
                                payload_id: row.get::<_, String>(0)?,
                                event_id: row.get::<_, String>(1)?,
                                activity_id: row.get::<_, Option<String>>(2)?,
                                payload_kind: row.get::<_, String>(3)?,
                                payload_json: row.get::<_, String>(4)?,
                            })
                        },
                    );
                    let result = match row {
                        Ok(record) => Ok(Some(record)),
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("Query returned no rows")
                                || msg.contains("query returned no rows")
                            {
                                Ok(None)
                            } else {
                                Err(graphqlite::Error::from(e))
                            }
                        }
                    };
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
                WorkerRequest::ReadPayloadByEventKind(event_id, payload_kind, reply) => {
                    let row = graph.connection().sqlite_connection().query_row(
                        SELECT_PAYLOAD_BY_EVENT_KIND_SQL,
                        (&event_id, &payload_kind),
                        |row| {
                            Ok(PayloadRecord {
                                payload_id: row.get::<_, String>(0)?,
                                event_id: row.get::<_, String>(1)?,
                                activity_id: row.get::<_, Option<String>>(2)?,
                                payload_kind: row.get::<_, String>(3)?,
                                payload_json: row.get::<_, String>(4)?,
                            })
                        },
                    );
                    let result = match row {
                        Ok(record) => Ok(Some(record)),
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("Query returned no rows")
                                || msg.contains("query returned no rows")
                            {
                                Ok(None)
                            } else {
                                Err(graphqlite::Error::from(e))
                            }
                        }
                    };
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
                WorkerRequest::UpsertPayload(payload, reply) => {
                    let sqlite = graph.connection().sqlite_connection();
                    let result = (|| {
                        sqlite.execute(
                            UPSERT_PAYLOAD_SQL,
                            (
                                &payload.payload_id,
                                &payload.event_id,
                                &payload.activity_id,
                                &payload.payload_kind,
                                &payload.payload_json,
                            ),
                        )?;
                        sqlite.execute(DELETE_PAYLOAD_FTS_SQL, (&payload.payload_id,))?;
                        sqlite.execute(
                            UPSERT_PAYLOAD_FTS_SQL,
                            (
                                &payload.payload_id,
                                &payload.event_id,
                                &payload.activity_id,
                                &payload.payload_kind,
                                &payload.payload_json,
                            ),
                        )?;
                        Ok::<(), graphqlite::Error>(())
                    })();
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
                WorkerRequest::SearchPayloadActivityIds(query_text, reply) => {
                    let mut stmt = match graph
                        .connection()
                        .sqlite_connection()
                        .prepare(SELECT_PAYLOAD_ACTIVITY_IDS_BY_TEXT_SQL)
                    {
                        Ok(stmt) => stmt,
                        Err(e) => {
                            let _ = reply.send(Err(graphqlite::Error::from(e)));
                            continue;
                        }
                    };
                    let mapped =
                        stmt.query_map([query_text.as_str()], |row| row.get::<_, String>(0));
                    let result = match mapped {
                        Ok(rows) => {
                            let mut out = Vec::new();
                            for value in rows.flatten() {
                                out.push(value);
                            }
                            Ok(out)
                        }
                        Err(e) => Err(graphqlite::Error::from(e)),
                    };
                    if reply.send(result).is_err() {
                        tracing::debug!(
                            "worker reply dropped (caller likely timed out or dropped)"
                        );
                    }
                }
            }
        }
    });
    let store = GraphqliteProvenanceStore {
        request_tx,
        normalizer: Arc::new(DefaultProvNormalizer::default()),
        mermaid_cache: config.mermaid_cache.clone(),
    };
    Ok(Arc::new(store))
}

impl GraphqliteProvenanceStore {
    /// Run a parameterized read Cypher query (no manual escaping).
    /// Serialized process-wide via CYPHER_REQUEST_SERIAL so only one Cypher runs at a time.
    async fn run_cypher_with_params(
        &self,
        query: &str,
        params: &QueryParams,
    ) -> Result<CypherResult> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::ReadWithParams(
                query.to_string(),
                params.clone(),
                reply_tx,
            ))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    /// Run a read-only Cypher query. Used by graph export (Mermaid, etc.).
    pub async fn run_cypher_read(&self, query: &str, params: &QueryParams) -> Result<CypherResult> {
        self.run_cypher_with_params(query, params).await
    }

    /// Run a parameterized read; exposed for tests.
    #[cfg(test)]
    pub(crate) async fn run_cypher_for_test_with_params(
        &self,
        query: &str,
        params: &QueryParams,
    ) -> Result<CypherResult> {
        self.run_cypher_with_params(query, params).await
    }

    /// Run a parameterized write Cypher query via the worker thread (no manual escaping).
    /// Serialized process-wide via CYPHER_REQUEST_SERIAL so only one Cypher runs at a time.
    async fn run_cypher_write(&self, query: &str, params: &QueryParams) -> Result<()> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::Write(
                query.to_string(),
                params.clone(),
                reply_tx,
            ))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    /// Run a write Cypher query through the Graph worker.
    pub async fn run_cypher_execute(&self, query: &str, params: &QueryParams) -> Result<()> {
        self.run_cypher_write(query, params).await
    }

    async fn upsert_payload(&self, payload: PayloadRecord) -> Result<()> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::UpsertPayload(payload, reply_tx))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    async fn read_payload_by_id(&self, payload_id: &str) -> Result<Option<PayloadRecord>> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::ReadPayloadById(
                payload_id.to_string(),
                reply_tx,
            ))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    async fn read_payload_by_event_kind(
        &self,
        event_id: &str,
        payload_kind: &str,
    ) -> Result<Option<PayloadRecord>> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::ReadPayloadByEventKind(
                event_id.to_string(),
                payload_kind.to_string(),
                reply_tx,
            ))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    async fn search_payload_activity_ids(&self, payload_text_query: &str) -> Result<Vec<String>> {
        let serial = CYPHER_REQUEST_SERIAL.get_or_init(|| TokioMutex::new(()));
        let _guard = serial.lock().await;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send(WorkerRequest::SearchPayloadActivityIds(
                payload_text_query.to_string(),
                reply_tx,
            ))
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        let result = reply_rx
            .await
            .map_err(|e| ProvenanceError::Storage(Box::new(e)))?;
        result.map_err(map_graphqlite_error)
    }

    async fn resolve_call_activity_id_for_event(&self, event_id: &str) -> Result<Option<String>> {
        let query = "MATCH (c) WHERE (c:LlmCall OR c:ToolCall) AND c.a2a_event_id = $event_id RETURN c.id AS activity_id LIMIT 1";
        let mut params = Map::new();
        params.insert("event_id".to_string(), Value::String(event_id.to_string()));
        let rows = self.run_cypher_read(query, &params).await?;
        let first = rows.iter().next();
        Ok(first.and_then(|row| row.get::<String>("activity_id").ok()))
    }

    /// Look up the task's agent by traversal: Task -[WAS_CREATED_BY]-> TaskExecution
    /// -[WAS_EXECUTED_BY]-> AgentRuntimeInstance. Parse agent_id from instance id.
    /// Returns None if the path is missing (e.g. TaskExecutionStarted not yet persisted).
    async fn get_task_agent_id(&self, task_id: &TaskId) -> Result<Option<AgentId>> {
        let task_entity_id = task_entity_id_string(task_id);
        let id_escaped = task_entity_id.replace('\'', "''");
        let task_label = GraphNodeLabel::Task.as_str();
        let agent_label = GraphNodeLabel::AgentRuntimeInstance.as_str();
        let query = format!(
            "MATCH (t:{task_label} {{id: '{id_escaped}'}})-[:WAS_CREATED_BY]->(te)-[:WAS_EXECUTED_BY]->(a) \
             WHERE a:{agent_label} RETURN a.id AS instance_id LIMIT 1"
        );
        let params = Map::new();
        let result = self.run_cypher_with_params(&query, &params).await?;
        let Some(row) = result.iter().next() else {
            return Ok(None);
        };
        let instance_id: String = row.get("instance_id").unwrap_or_default();
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
}

#[async_trait]
impl ProvenanceWriter for GraphqliteProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        let _start = Instant::now();
        validate_event(&event)?;
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
        let context_id_opt = event.context_id_opt().map(|c| c.as_str());
        let statements = cypher_build::build_queries_with_key_style_params(
            &normalized,
            KeyStyle::StorageSafeUnderscore,
            context_id_opt,
        );

        for stmt in &statements {
            let params = require_object_params(&stmt.params)?;
            self.run_cypher_write(&stmt.query, &params).await?;
        }
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

        if let (Some(cache), Some(ctx)) = (&self.mermaid_cache, context_id_opt) {
            cache.invalidate(ctx);
        }
        Ok(())
    }
}

#[async_trait]
impl ProvenanceContextReader for GraphqliteProvenanceStore {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        let context = context_id.as_str();
        let (query, params) = ConversationReadModel::message_query_storage_safe_params(context);
        let params = require_object_params(&params)?;
        let results = self.run_cypher_with_params(&query, &params).await?;
        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in results.iter() {
            let msg = match MessageRow::from_row(row) {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Role is stored in the graph in canonical enum form (ROLE_USER, ROLE_AGENT); use as-is.
            let role = msg.role;
            let content = normalize_message_content(&msg.content);
            if content.trim().is_empty() {
                continue;
            }
            messages.push(ProvenanceContextMessage {
                message_id: MessageId::from(msg.message_id.as_str()),
                timestamp_ms: event_id_to_timestamp_ms(&msg.event_id),
                role,
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
        let context = context_id.as_str();
        let (message_query, message_params) =
            ConversationReadModel::message_query_storage_safe_params(context);
        let (tool_query, tool_params) =
            ConversationReadModel::tool_query_storage_safe_params(context);
        let message_params = require_object_params(&message_params)?;
        let tool_params = require_object_params(&tool_params)?;

        let message_results = self
            .run_cypher_with_params(&message_query, &message_params)
            .await?;
        let tool_results = self
            .run_cypher_with_params(&tool_query, &tool_params)
            .await?;

        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in message_results.iter() {
            let msg = match MessageRow::from_row(row) {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Role is stored in the graph in canonical enum form (ROLE_USER, ROLE_AGENT); use as-is.
            let role = msg.role;
            let content = normalize_message_content(&msg.content);
            if content.trim().is_empty() {
                continue;
            }
            items.push(ProvenanceConversationContextItem {
                timestamp_ms: event_id_to_timestamp_ms(&msg.event_id),
                event_id: EventId::from(msg.event_id.as_str()),
                agent_id: msg.agent_id.clone(),
                role,
                content: Value::String(content),
                source: "message".to_string(),
            });
        }

        for row in tool_results.iter() {
            let tool = match ToolCallRow::from_row(row) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !tool.contract_holds() {
                continue;
            }
            if !tool.is_completed() {
                continue;
            }
            let tool_call_payload = self
                .read_payload_by_event_kind(&tool.event_id, "tool_call")
                .await?;
            let tool_result_payload = self
                .read_payload_by_event_kind(&tool.event_id, "tool_result")
                .await?;
            let (args, phase) = if let Some(payload) = tool_call_payload {
                let parsed = parse_json_like_string(&payload.payload_json);
                let args = parsed
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| tool.args.clone());
                let phase_label = parsed
                    .get("phase")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default();
                let phase = ToolSessionPhase::from_metadata(&serde_json::json!({
                    "phase": phase_label
                }));
                (args, phase)
            } else {
                let metadata = tool.metadata.clone();
                (
                    tool.args.clone(),
                    ToolSessionPhase::from_metadata(&metadata),
                )
            };
            let (result, error) = if let Some(payload) = tool_result_payload {
                let parsed = parse_json_like_string(&payload.payload_json);
                let result = parsed
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| parsed.clone());
                let error = parsed.get("error").cloned();
                (result, error)
            } else {
                let metadata = tool.metadata.clone();
                let result = metadata
                    .get("result")
                    .cloned()
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                (result, metadata_error(&metadata))
            };
            let has_outcome = has_meaningful_result(&result) || error.is_some();
            let include_call = !matches!(
                phase,
                ToolSessionPhase::Open | ToolSessionPhase::Finish | ToolSessionPhase::Abort
            ) && (!is_empty_object(&args) || has_outcome);

            if include_call {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_to_timestamp_ms(&tool.event_id),
                    event_id: EventId::from(tool.event_id.as_str()),
                    agent_id: tool.agent_id.clone(),
                    role: "assistant".to_string(),
                    content: serde_json::json!({
                        "tool_call": {
                            "name": tool.tool_name,
                            "args": args,
                            "fsm_phase": phase.label()
                        }
                    }),
                    source: "tool_call".to_string(),
                });
            }

            if include_call && has_outcome {
                let mut content = serde_json::Map::new();
                content.insert("tool_name".to_string(), Value::String(tool.tool_name));
                content.insert(
                    "fsm_phase".to_string(),
                    Value::String(phase.label().to_string()),
                );
                if has_meaningful_result(&result) {
                    content.insert("result".to_string(), result);
                }
                if let Some(error) = error {
                    content.insert("error".to_string(), error);
                }
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_to_timestamp_ms(&tool.event_id),
                    event_id: EventId::from(tool.event_id.as_str()),
                    agent_id: tool.agent_id.clone(),
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

/// API-exposed reads: no guarantee of no-stale-read. Same worker path as [ProvenanceContextReader];
/// the type system enforces that API callers use this trait, not the consistent-read trait.
#[async_trait]
impl ProvenanceQueryApi for GraphqliteProvenanceStore {
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

fn value_from_row(row: &Row, col: &str) -> Value {
    if let Some(value) = row.get_value(col) {
        return graphqlite_value_to_json(value);
    }
    if let Ok(v) = row.get::<String>(col) {
        return Value::String(v);
    }
    if let Ok(v) = row.get::<i64>(col) {
        return Value::Number(v.into());
    }
    if let Ok(v) = row.get::<bool>(col) {
        return Value::Bool(v);
    }
    if let Ok(v) = row.get::<f64>(col)
        && let Some(n) = serde_json::Number::from_f64(v)
    {
        return Value::Number(n);
    }
    Value::Null
}

fn cypher_result_to_json_rows(rows: &CypherResult) -> Vec<Map<String, Value>> {
    let cols = rows.columns();
    rows.iter()
        .map(|row| {
            let mut out = Map::new();
            for col in cols {
                out.insert(col.to_string(), value_from_row(row, col));
            }
            out
        })
        .collect()
}

fn parse_json_field(row: &Map<String, Value>, field: &str) -> Option<Value> {
    let raw = row.get(field)?;
    match raw {
        Value::String(s) if !s.trim().is_empty() => Some(parse_json_like_string(s)),
        Value::Object(_) | Value::Array(_) => Some(raw.clone()),
        _ => None,
    }
}

fn payload_text_match_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn agent_id_from_instance_id(instance_id: &str) -> Option<String> {
    let raw = instance_id.strip_prefix("agent_instance:")?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

fn normalize_agent_field(raw: Option<&str>, fallback: &str) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .unwrap_or(fallback)
        .to_string()
}

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

fn parse_activity_outcome(row: &Map<String, Value>) -> NodeActivityOutcome {
    let raw = row
        .get("activity_outcome")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    match raw {
        "Success" => NodeActivityOutcome::Success,
        "Failed" => NodeActivityOutcome::Failed,
        "InProgress" => NodeActivityOutcome::InProgress,
        _ => NodeActivityOutcome::InProgress,
    }
}

fn apply_activity_lifecycle_fields(row: &mut Map<String, Value>) -> NodeActivityOutcome {
    let outcome = parse_activity_outcome(row);
    let status = if outcome.is_completed() {
        "Completed"
    } else {
        "InProgress"
    };
    let outcome_label = match outcome {
        NodeActivityOutcome::Success => "Success",
        NodeActivityOutcome::Failed => "Failed",
        NodeActivityOutcome::InProgress => "Indeterminate",
    };
    row.insert(
        "activity_status".to_string(),
        Value::String(status.to_string()),
    );
    row.insert(
        "activity_outcome".to_string(),
        Value::String(outcome_label.to_string()),
    );
    outcome
}

fn row_is_failed(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Failed")
}

fn row_is_success(row: &Map<String, Value>) -> bool {
    row.get("activity_outcome").and_then(Value::as_str) == Some("Success")
}

fn row_timestamp_ms(row: &Map<String, Value>) -> u64 {
    row.get("timestamp_ms").and_then(Value::as_u64).unwrap_or(0)
}

fn value_weight(value: Option<&Value>) -> usize {
    match value {
        None | Some(Value::Null) => 0,
        Some(Value::String(s)) => s.len(),
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(map)) => map.len(),
        Some(Value::Bool(_)) | Some(Value::Number(_)) => 1,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpsField {
    ActivityId,
    ActivityKind,
    TimestampMs,
    DurationMs,
    TotalTokens,
    PromptTokens,
    CompletionTokens,
    AgentId,
    AgentDisplay,
    AgentPackage,
    AgentVersion,
    ContextId,
    TaskId,
    MessageId,
    Provider,
    Model,
    ToolName,
    BamlPrompt,
    Role,
    ActivityOutcome,
    ActivityStatus,
    FailureClass,
    FailureEvidence,
    TotalProcessingMs,
    LlmDurationMsSum,
    ToolDurationMsSum,
}

impl OpsField {
    fn key(self) -> &'static str {
        match self {
            Self::ActivityId => "activity_id",
            Self::ActivityKind => "activity_kind",
            Self::TimestampMs => "timestamp_ms",
            Self::DurationMs => "duration_ms",
            Self::TotalTokens => "total_tokens",
            Self::PromptTokens => "prompt_tokens",
            Self::CompletionTokens => "completion_tokens",
            Self::AgentId => "agent_id",
            Self::AgentDisplay => "agent_display",
            Self::AgentPackage => "agent_package",
            Self::AgentVersion => "agent_version",
            Self::ContextId => "context_id",
            Self::TaskId => "task_id",
            Self::MessageId => "message_id",
            Self::Provider => "provider",
            Self::Model => "model",
            Self::ToolName => "tool_name",
            Self::BamlPrompt => "baml_prompt",
            Self::Role => "role",
            Self::ActivityOutcome => "activity_outcome",
            Self::ActivityStatus => "activity_status",
            Self::FailureClass => "failure_class",
            Self::FailureEvidence => "failure_evidence",
            Self::TotalProcessingMs => "total_processing_ms",
            Self::LlmDurationMsSum => "llm_duration_ms_sum",
            Self::ToolDurationMsSum => "tool_duration_ms_sum",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "activity_id" => Some(Self::ActivityId),
            "activity_kind" => Some(Self::ActivityKind),
            "timestamp_ms" => Some(Self::TimestampMs),
            "duration_ms" => Some(Self::DurationMs),
            "total_tokens" => Some(Self::TotalTokens),
            "prompt_tokens" => Some(Self::PromptTokens),
            "completion_tokens" => Some(Self::CompletionTokens),
            "agent_id" => Some(Self::AgentId),
            "agent_display" => Some(Self::AgentDisplay),
            "agent_package" => Some(Self::AgentPackage),
            "agent_version" => Some(Self::AgentVersion),
            "context_id" => Some(Self::ContextId),
            "task_id" => Some(Self::TaskId),
            "message_id" => Some(Self::MessageId),
            "provider" => Some(Self::Provider),
            "model" => Some(Self::Model),
            "tool_name" => Some(Self::ToolName),
            "baml_prompt" => Some(Self::BamlPrompt),
            "role" => Some(Self::Role),
            "activity_outcome" => Some(Self::ActivityOutcome),
            "activity_status" => Some(Self::ActivityStatus),
            "failure_class" => Some(Self::FailureClass),
            "failure_evidence" => Some(Self::FailureEvidence),
            "total_processing_ms" => Some(Self::TotalProcessingMs),
            "llm_duration_ms_sum" => Some(Self::LlmDurationMsSum),
            "tool_duration_ms_sum" => Some(Self::ToolDurationMsSum),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpsSortDir {
    Asc,
    Desc,
}

impl OpsSortDir {
    fn is_desc(self) -> bool {
        matches!(self, Self::Desc)
    }
}

fn parse_ops_sort_field(raw: Option<&str>) -> Result<OpsField> {
    let field = raw.unwrap_or("timestamp_ms");
    OpsField::parse(field).ok_or_else(|| ProvenanceError::InvalidEvent {
        event_id: "ops_query".to_string(),
        reason: format!("unsupported sort field: {field}"),
    })
}

fn parse_ops_sort_dir(raw: Option<&str>) -> Result<OpsSortDir> {
    match raw.unwrap_or("desc") {
        "asc" | "ASC" => Ok(OpsSortDir::Asc),
        "desc" | "DESC" => Ok(OpsSortDir::Desc),
        other => Err(ProvenanceError::InvalidEvent {
            event_id: "ops_query".to_string(),
            reason: format!("unsupported sort direction: {other}"),
        }),
    }
}

fn parse_ops_group_by(raw: &[String]) -> Result<Vec<OpsField>> {
    if raw.is_empty() {
        return Ok(vec![OpsField::AgentId]);
    }
    raw.iter()
        .map(|field| {
            OpsField::parse(field).ok_or_else(|| ProvenanceError::InvalidEvent {
                event_id: "ops_query".to_string(),
                reason: format!("unsupported group dimension: {field}"),
            })
        })
        .collect()
}

fn finalize_call_row(
    row: &mut Map<String, Value>,
    identity_by_agent_id: &HashMap<String, (String, String)>,
    failure_by_activity_id: &HashMap<String, (String, String)>,
    activity_kind: &'static str,
) -> Result<()> {
    apply_agent_identity_fields(row, identity_by_agent_id);
    let activity_outcome = apply_activity_lifecycle_fields(row);
    if activity_outcome == NodeActivityOutcome::Failed {
        let activity_id = row
            .get("activity_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                event_id: "ops_query".to_string(),
                reason: format!("missing activity_id for failed {activity_kind}"),
            })?
            .to_string();
        let resolved = failure_by_activity_id.get(&activity_id).ok_or_else(|| {
            ProvenanceError::InvalidEvent {
                event_id: activity_id,
                reason: format!(
                    "missing write-time failure classification for failed {activity_kind}"
                ),
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
    let ts = row.get("timestamp_ms").and_then(Value::as_u64).unwrap_or(0);
    row.insert("timestamp_ms".to_string(), Value::Number(ts.into()));
    row.insert(
        "activity_kind".to_string(),
        Value::String(activity_kind.to_string()),
    );
    Ok(())
}

fn apply_common_filters(
    mut rows: Vec<Map<String, Value>>,
    req: &ProvenanceOpsQueryRequest,
) -> Vec<Map<String, Value>> {
    let prompt_filter_lc = req
        .filters
        .baml_prompt
        .as_ref()
        .map(|prompt| prompt.to_ascii_lowercase());
    let outcome_segment = req
        .outcome
        .clone()
        .unwrap_or(ProvenanceOutcomeSegment::Both);
    rows.retain(|row| {
        if let Some(from_ms) = req.filters.from_timestamp_ms
            && row_timestamp_ms(row) < from_ms
        {
            return false;
        }
        if let Some(to_ms) = req.filters.to_timestamp_ms
            && row_timestamp_ms(row) > to_ms
        {
            return false;
        }
        if let Some(ref provider) = req.filters.provider
            && row.get("provider").and_then(Value::as_str) != Some(provider.as_str())
        {
            return false;
        }
        if let Some(ref model) = req.filters.model
            && row.get("model").and_then(Value::as_str) != Some(model.as_str())
        {
            return false;
        }
        if let Some(ref tool_name) = req.filters.tool_name
            && row.get("tool_name").and_then(Value::as_str) != Some(tool_name.as_str())
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
        if let Some(ref agent_id) = req.filters.agent_id
            && row.get("agent_id").and_then(Value::as_str) != Some(agent_id.as_str())
        {
            return false;
        }
        match outcome_segment {
            ProvenanceOutcomeSegment::FailedOnly => {
                if !row_is_failed(row) {
                    return false;
                }
            }
            ProvenanceOutcomeSegment::SuccessfulOnly => {
                if !row_is_success(row) {
                    return false;
                }
            }
            ProvenanceOutcomeSegment::Both => {
                // "both" includes completed outcomes only; excludes indeterminate in-progress rows.
                if !row_is_success(row) && !row_is_failed(row) {
                    return false;
                }
            }
        }
        true
    });
    rows
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
    group_by: &[OpsField],
    top_k: usize,
) -> Vec<Value> {
    type HotspotAggregate = (Vec<Option<String>>, u64, u64, u64, u64);

    if rows.is_empty() {
        return Vec::new();
    }
    let dims: Vec<&str> = group_by.iter().map(|d| d.key()).collect();
    let mut groups: std::collections::HashMap<String, HotspotAggregate> =
        std::collections::HashMap::new();
    for row in rows {
        let group_values: Vec<Option<String>> = dims
            .iter()
            .map(|d| {
                row.get(*d).and_then(|v| match v {
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
        let failed = u64::from(row_is_failed(row));
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
                    "groupDimensions": dims.clone(),
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

impl GraphqliteProvenanceStore {
    async fn load_agent_identity_map(&self) -> Result<HashMap<String, (String, String)>> {
        let rows = self
            .run_cypher_read(
                "MATCH (a:AgentRuntimeInstance) \
                 RETURN a.id AS instance_id, toString(a.a2a_agent_type) AS agent_package, \
                        toString(a.a2a_agent_version) AS agent_version",
                &Map::new(),
            )
            .await?;
        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in cypher_result_to_json_rows(&rows) {
            let instance_id = row
                .get("instance_id")
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

    async fn load_failure_classification_map(&self) -> Result<HashMap<String, (String, String)>> {
        let rows = self
            .run_cypher_read(
                "MATCH (call)-[used:WAS_USED_BY]->(fc:FailureClassification) \
                 WHERE (call:LlmCall OR call:ToolCall) \
                 RETURN toString(call.id) AS activity_id, \
                        toString(fc.a2a_failure_class) AS failure_class, \
                        toString(fc.a2a_failure_evidence) AS failure_evidence",
                &Map::new(),
            )
            .await?;
        let mut out: HashMap<String, (String, String)> = HashMap::new();
        for row in cypher_result_to_json_rows(&rows) {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if activity_id.is_empty() || activity_id == "null" {
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
            if let Some(existing) = out.get(&activity_id) {
                let incoming = (class.clone(), evidence.clone());
                if existing != &incoming {
                    return Err(ProvenanceError::InvalidEvent {
                        event_id: activity_id,
                        reason: format!(
                            "multiple conflicting failure classifications for activity: existing=({}, {}), incoming=({}, {})",
                            existing.0, existing.1, incoming.0, incoming.1
                        ),
                    });
                }
            } else {
                out.insert(activity_id, (class, evidence));
            }
        }
        Ok(out)
    }

    async fn query_llm_rows(
        &self,
        req: &ProvenanceOpsQueryRequest,
    ) -> Result<Vec<Map<String, Value>>> {
        let identity_by_agent_id = self.load_agent_identity_map().await?;
        let failure_by_activity_id = self.load_failure_classification_map().await?;
        let payload_text_activity_filter: Option<HashSet<String>> =
            if let Some(payload_text) = req.filters.payload_text.as_deref() {
                let fts_query = payload_text_match_query(payload_text);
                if fts_query.is_empty() {
                    None
                } else {
                    Some(
                        self.search_payload_activity_ids(&fts_query)
                            .await?
                            .into_iter()
                            .collect(),
                    )
                }
            } else {
                None
            };
        let mut where_parts: Vec<&str> = vec!["1=1"];
        let mut params = Map::new();
        if let Some(ref context_id) = req.filters.context_id {
            where_parts.push("c.a2a_context_id = $context_id");
            params.insert(
                "context_id".to_string(),
                Value::String(context_id.as_str().to_string()),
            );
        }
        if let Some(ref task_id) = req.filters.task_id {
            where_parts.push("c.a2a_task_id = $task_id");
            params.insert(
                "task_id".to_string(),
                Value::String(task_id.as_str().to_string()),
            );
        }
        let query = format!(
            "MATCH (c:LlmCall) WHERE {} \
             OPTIONAL MATCH (c)-[:WAS_USED_BY]->(p:LlmPrompt) \
             RETURN c.id AS activity_id, \
                    coalesce(c.prov_endTime, c.prov_startTime, 0) AS timestamp_ms, \
                    c.a2a_context_id AS context_id, c.a2a_task_id AS task_id, \
                    c.a2a_message_id AS message_id, c.a2a_agent_id AS agent_id, \
                    c.a2a_client AS provider, c.a2a_model AS model, c.a2a_function_name AS baml_prompt, \
                    c.a2a_duration_ms AS duration_ms, c.a2a_usage_prompt_tokens AS prompt_tokens, \
                    c.a2a_usage_completion_tokens AS completion_tokens, c.a2a_usage_total_tokens AS total_tokens, \
                    c.a2a_drift_score AS drift_score, c.a2a_drift_severity AS drift_severity, \
                    c.a2a_drift_mode AS drift_mode, c.a2a_drift_warn_min_score AS drift_warn_min_score, \
                    c.a2a_drift_block_min_score AS drift_block_min_score, \
                    c.a2a_intent_text_preview AS intent_text_preview, \
                    c.a2a_response_text_preview AS response_text_preview, \
                    c.a2a_activity_outcome AS activity_outcome, c.a2a_result AS llm_result_raw, \
                    c.a2a_error AS llm_error_raw, \
                    p.a2a_prompt AS llm_call, \
                    c.a2a_llm_call_payload_id AS llm_call_payload_id, \
                    c.a2a_llm_result_payload_id AS llm_result_payload_id \
             ORDER BY c.id",
            where_parts.join(" AND ")
        );
        let rows = self.run_cypher_read(&query, &params).await?;
        let mut by_activity_id: std::collections::HashMap<String, Map<String, Value>> =
            std::collections::HashMap::new();
        for row in cypher_result_to_json_rows(&rows) {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if activity_id.is_empty() {
                continue;
            }
            if let Some(filter_ids) = payload_text_activity_filter.as_ref()
                && !filter_ids.contains(&activity_id)
            {
                continue;
            }
            let incoming_call_weight = value_weight(row.get("llm_call"));
            let replace = by_activity_id
                .get(&activity_id)
                .map(|existing| {
                    let existing_call_weight = value_weight(existing.get("llm_call"));
                    incoming_call_weight > existing_call_weight
                })
                .unwrap_or(true);
            if replace {
                by_activity_id.insert(activity_id, row);
            }
        }
        let mut out: Vec<Map<String, Value>> = by_activity_id.into_values().collect();
        out.sort_by(|a, b| {
            let av = a
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let bv = b
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            av.cmp(bv)
        });
        for row in &mut out {
            let llm_call =
                if let Some(payload_id) = row.get("llm_call_payload_id").and_then(Value::as_str) {
                    self.read_payload_by_id(payload_id)
                        .await?
                        .map(|payload| parse_json_like_string(&payload.payload_json))
                        .or_else(|| parse_json_field(row, "llm_call"))
                        .unwrap_or(Value::Null)
                } else {
                    parse_json_field(row, "llm_call").unwrap_or(Value::Null)
                };
            row.insert("llm_call".to_string(), llm_call);
            let llm_result = if let Some(payload_id) =
                row.get("llm_result_payload_id").and_then(Value::as_str)
            {
                self.read_payload_by_id(payload_id)
                    .await?
                    .map(|payload| parse_json_like_string(&payload.payload_json))
                    .unwrap_or(Value::Null)
            } else {
                let llm_result_value = parse_json_field(row, "llm_result_raw");
                let llm_error_value = parse_json_field(row, "llm_error_raw");
                match (llm_result_value, llm_error_value) {
                    (Some(result), Some(error)) => serde_json::json!({
                        "result": result,
                        "error": error
                    }),
                    (Some(result), None) => result,
                    (None, Some(error)) => serde_json::json!({
                        "error": error
                    }),
                    (None, None) => Value::Null,
                }
            };
            row.insert("llm_result".to_string(), llm_result);
            row.remove("llm_result_raw");
            row.remove("llm_error_raw");
            row.remove("llm_call_payload_id");
            row.remove("llm_result_payload_id");
            nest_llm_drift_fields(row);
            finalize_call_row(
                row,
                &identity_by_agent_id,
                &failure_by_activity_id,
                "llm_call",
            )?;
        }
        Ok(apply_common_filters(out, req))
    }

    async fn query_tool_rows(
        &self,
        req: &ProvenanceOpsQueryRequest,
    ) -> Result<Vec<Map<String, Value>>> {
        let identity_by_agent_id = self.load_agent_identity_map().await?;
        let failure_by_activity_id = self.load_failure_classification_map().await?;
        let payload_text_activity_filter: Option<HashSet<String>> =
            if let Some(payload_text) = req.filters.payload_text.as_deref() {
                let fts_query = payload_text_match_query(payload_text);
                if fts_query.is_empty() {
                    None
                } else {
                    Some(
                        self.search_payload_activity_ids(&fts_query)
                            .await?
                            .into_iter()
                            .collect(),
                    )
                }
            } else {
                None
            };
        let mut where_parts: Vec<&str> = vec!["1=1"];
        let mut params = Map::new();
        if let Some(ref context_id) = req.filters.context_id {
            where_parts.push("t.a2a_context_id = $context_id");
            params.insert(
                "context_id".to_string(),
                Value::String(context_id.as_str().to_string()),
            );
        }
        if let Some(ref task_id) = req.filters.task_id {
            where_parts.push("t.a2a_task_id = $task_id");
            params.insert(
                "task_id".to_string(),
                Value::String(task_id.as_str().to_string()),
            );
        }
        let query = format!(
            "MATCH (t:ToolCall) WHERE {} \
             OPTIONAL MATCH (t)-[:WAS_USED_BY]->(args:ToolArgs) \
             RETURN t.id AS activity_id, \
                    coalesce(t.prov_endTime, t.prov_startTime, 0) AS timestamp_ms, \
                    t.a2a_context_id AS context_id, t.a2a_task_id AS task_id, \
                    t.a2a_message_id AS message_id, t.a2a_agent_id AS agent_id, \
                    t.a2a_tool_name AS tool_name, t.a2a_function_name AS baml_prompt, \
                    t.a2a_duration_ms AS duration_ms, t.a2a_activity_outcome AS activity_outcome, \
                    t.a2a_phase AS phase, t.a2a_result AS tool_result_raw, \
                    t.a2a_error AS tool_error_raw, args.a2a_args AS tool_args, \
                    t.a2a_tool_call_payload_id AS tool_call_payload_id, \
                    t.a2a_tool_result_payload_id AS tool_result_payload_id \
             ORDER BY t.id",
            where_parts.join(" AND ")
        );
        let rows = self.run_cypher_read(&query, &params).await?;
        let mut by_activity_id: std::collections::HashMap<String, Map<String, Value>> =
            std::collections::HashMap::new();
        for row in cypher_result_to_json_rows(&rows) {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if activity_id.is_empty() {
                continue;
            }
            if let Some(filter_ids) = payload_text_activity_filter.as_ref()
                && !filter_ids.contains(&activity_id)
            {
                continue;
            }
            let incoming_args_weight = value_weight(row.get("tool_args"));
            let replace = by_activity_id
                .get(&activity_id)
                .map(|existing| {
                    let existing_args_weight = value_weight(existing.get("tool_args"));
                    incoming_args_weight > existing_args_weight
                })
                .unwrap_or(true);
            if replace {
                by_activity_id.insert(activity_id, row);
            }
        }
        let mut out: Vec<Map<String, Value>> = by_activity_id.into_values().collect();
        out.sort_by(|a, b| {
            let av = a
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let bv = b
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            av.cmp(bv)
        });
        for row in &mut out {
            let tool_name = row
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let (tool_args, tool_phase) =
                if let Some(payload_id) = row.get("tool_call_payload_id").and_then(Value::as_str) {
                    if let Some(payload) = self.read_payload_by_id(payload_id).await? {
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
                            parse_json_field(row, "tool_args").unwrap_or(Value::Null),
                            row.get("phase")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string),
                        )
                    }
                } else {
                    (
                        parse_json_field(row, "tool_args").unwrap_or(Value::Null),
                        row.get("phase")
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
            let tool_result = if let Some(payload_id) =
                row.get("tool_result_payload_id").and_then(Value::as_str)
            {
                self.read_payload_by_id(payload_id)
                    .await?
                    .map(|payload| parse_json_like_string(&payload.payload_json))
                    .unwrap_or(Value::Null)
            } else {
                let tool_result_value = parse_json_field(row, "tool_result_raw");
                let tool_error_value = parse_json_field(row, "tool_error_raw");
                match (tool_result_value, tool_error_value) {
                    (Some(result), Some(error)) => serde_json::json!({
                        "result": result,
                        "error": error
                    }),
                    (Some(result), None) => result,
                    (None, Some(error)) => serde_json::json!({
                        "error": error
                    }),
                    (None, None) => Value::Null,
                }
            };
            if tool_phase.as_deref() == Some("open") {
                continue;
            }
            row.insert("tool_result".to_string(), tool_result);
            row.remove("tool_args");
            row.remove("phase");
            row.remove("tool_result_raw");
            row.remove("tool_error_raw");
            row.remove("tool_call_payload_id");
            row.remove("tool_result_payload_id");
            finalize_call_row(
                row,
                &identity_by_agent_id,
                &failure_by_activity_id,
                "tool_call",
            )?;
        }
        let mut by_activity_id: std::collections::HashMap<String, Map<String, Value>> =
            std::collections::HashMap::new();
        for row in out {
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if activity_id.is_empty() {
                continue;
            }
            let incoming_has_result = match row.get("tool_result") {
                None | Some(Value::Null) => false,
                Some(Value::Object(map)) if map.is_empty() => false,
                _ => true,
            };
            let incoming_ts = row
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let replace = by_activity_id
                .get(&activity_id)
                .map(|existing| {
                    let existing_has_result = match existing.get("tool_result") {
                        None | Some(Value::Null) => false,
                        Some(Value::Object(map)) if map.is_empty() => false,
                        _ => true,
                    };
                    let existing_ts = existing
                        .get("timestamp_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    (incoming_has_result, incoming_ts) > (existing_has_result, existing_ts)
                })
                .unwrap_or(true);
            if replace {
                by_activity_id.insert(activity_id, row);
            }
        }
        let mut collapsed: Vec<Map<String, Value>> = by_activity_id.into_values().collect();
        collapsed.sort_by(|a, b| {
            let av = a
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let bv = b
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            av.cmp(&bv)
        });
        Ok(apply_common_filters(collapsed, req))
    }

    async fn query_message_rows(
        &self,
        req: &ProvenanceOpsQueryRequest,
    ) -> Result<Vec<Map<String, Value>>> {
        let identity_by_agent_id = self.load_agent_identity_map().await?;
        let mut where_parts: Vec<&str> = vec!["1=1"];
        let mut params = Map::new();
        if let Some(ref context_id) = req.filters.context_id {
            where_parts.push("m.a2a_context_id = $context_id");
            params.insert(
                "context_id".to_string(),
                Value::String(context_id.as_str().to_string()),
            );
        }
        let query = format!(
            "MATCH (m:Message) WHERE {} \
             RETURN m.id AS activity_id, coalesce(m.prov_time, 0) AS timestamp_ms, \
                    m.a2a_context_id AS context_id, m.a2a_task_id AS task_id, \
                    m.a2a_message_id AS message_id, m.a2a_agent_id AS agent_id, \
                    m.a2a_role AS role, m.a2a_direction AS direction, m.a2a_content AS content \
             ORDER BY m.id",
            where_parts.join(" AND ")
        );
        let rows = self.run_cypher_read(&query, &params).await?;
        let mut out = cypher_result_to_json_rows(&rows);
        let mut llm_duration_by_message: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut tool_duration_by_message: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        if let Some(context_id) = req.filters.context_id.clone() {
            let mut m = Map::new();
            m.insert(
                "context_id".to_string(),
                Value::String(context_id.as_str().to_string()),
            );
            let llm_rows = self
                .run_cypher_read(crate::context_metrics_queries::TURN_TOTALS_BY_CONTEXT, &m)
                .await?;
            for row in cypher_result_to_json_rows(&llm_rows) {
                if let Some(message_id) = row.get("message_id").and_then(Value::as_str) {
                    llm_duration_by_message.insert(
                        message_id.to_string(),
                        row.get("llm_duration_ms_total")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                }
            }
            let tool_rows = self
                .run_cypher_read(
                    "MATCH (m:A2AMessageProcessing)-[:WAS_EXECUTED_BY]->(t:ToolCall) \
                     WHERE m.a2a_context_id = $context_id AND t.a2a_duration_ms IS NOT NULL \
                     RETURN m.a2a_message_id AS message_id, sum(t.a2a_duration_ms) AS tool_duration_ms_total",
                    &m,
                )
                .await?;
            for row in cypher_result_to_json_rows(&tool_rows) {
                if let Some(message_id) = row.get("message_id").and_then(Value::as_str) {
                    tool_duration_by_message.insert(
                        message_id.to_string(),
                        row.get("tool_duration_ms_total")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    );
                }
            }
        }
        for row in &mut out {
            let message_id = row
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
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
            row.insert(
                "timestamp_ms".to_string(),
                Value::Number(
                    row.get("timestamp_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        .into(),
                ),
            );
            let message_content = parse_json_field(row, "content").unwrap_or(Value::Array(vec![]));
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
            row.remove("content");
            apply_agent_identity_fields(row, &identity_by_agent_id);
            row.insert(
                "activity_status".to_string(),
                Value::String("Completed".to_string()),
            );
            row.insert(
                "activity_outcome".to_string(),
                Value::String("Success".to_string()),
            );
            row.insert(
                "activity_kind".to_string(),
                Value::String("message_turn".to_string()),
            );
            let activity_id = row
                .get("activity_id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    event_id: "ops_query".to_string(),
                    reason: "message row missing canonical activity_id".to_string(),
                })?
                .to_string();
            row.insert("activity_id".to_string(), Value::String(activity_id));
        }
        Ok(apply_common_filters(out, req))
    }
}

fn nest_llm_drift_fields(row: &mut Map<String, Value>) {
    let drift_score = row.remove("drift_score");
    let drift_severity = row.remove("drift_severity");
    let drift_mode = row.remove("drift_mode");
    let drift_warn_min_score = row.remove("drift_warn_min_score");
    let drift_block_min_score = row.remove("drift_block_min_score");
    let intent_text_preview = row.remove("intent_text_preview");
    let response_text_preview = row.remove("response_text_preview");

    let has_any = drift_score.is_some()
        || drift_severity.is_some()
        || drift_mode.is_some()
        || drift_warn_min_score.is_some()
        || drift_block_min_score.is_some()
        || intent_text_preview.is_some()
        || response_text_preview.is_some();
    if !has_any {
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
    if !drift.is_empty() {
        row.insert("drift".to_string(), Value::Object(drift));
    }
}

#[async_trait]
impl ProvenanceOpsQuery for GraphqliteProvenanceStore {
    async fn query_ops(
        &self,
        mut request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse> {
        let sort_field = parse_ops_sort_field(request.sort_by.as_deref())?;
        let sort_dir = parse_ops_sort_dir(request.sort_dir.as_deref())?;
        let group_dims = parse_ops_group_by(&request.group_by)?;
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

        let mut rows = match request.resource {
            ProvenanceOpsResource::LlmCalls => self.query_llm_rows(&request).await?,
            ProvenanceOpsResource::ToolCalls => self.query_tool_rows(&request).await?,
            ProvenanceOpsResource::Messages => self.query_message_rows(&request).await?,
            ProvenanceOpsResource::Aggregates => {
                // Default aggregate scope is LLM calls unless caller explicitly sets group dimensions
                // and filters that target other rows; we still compute from LLM rows for token budgets.
                self.query_llm_rows(&request).await?
            }
        };

        let sort_by = sort_field.key();
        let sort_desc = sort_dir.is_desc();
        rows.sort_by(|a, b| {
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

        let mut durations: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.get("duration_ms").and_then(Value::as_f64))
            .collect();
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut tokens: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.get("total_tokens").and_then(Value::as_f64))
            .collect();
        tokens.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let duration_p95 = percentile(&durations, 0.95);
        let duration_p99 = percentile(&durations, 0.99);
        let token_p95 = percentile(&tokens, 0.95);
        let token_p99 = percentile(&tokens, 0.99);

        let total_rows = rows.len();
        let page_end = std::cmp::min(offset.saturating_add(page_size), total_rows);
        let page_rows = if offset < total_rows {
            rows[offset..page_end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if page_end < total_rows {
            Some(page_end.to_string())
        } else {
            None
        };

        let top_k = request.top_k.unwrap_or(10) as usize;
        let hotspot_groups = build_hotspot_groups(&rows, &group_dims, top_k);
        let failed_count = rows.iter().filter(|r| row_is_failed(r)).count();
        let total_tokens_sum: u64 = rows
            .iter()
            .map(|r| r.get("total_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let prompt_tokens_sum: u64 = rows
            .iter()
            .map(|r| r.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0))
            .sum();
        let completion_tokens_sum: u64 = rows
            .iter()
            .map(|r| {
                r.get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .sum();
        let total_duration_sum: u64 = rows
            .iter()
            .map(|r| r.get("duration_ms").and_then(Value::as_u64).unwrap_or(0))
            .sum();

        Ok(ProvenanceOpsQueryResponse {
            resource: request.resource,
            rows: page_rows.into_iter().map(Value::Object).collect(),
            summary: serde_json::json!({
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
            }),
            hotspot_groups,
            next_cursor,
            truncated: total_rows > page_size || requested_page > page_cap,
            applied_caps: serde_json::Map::from_iter([
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
}

/// Builder for the GraphQLite provenance store. Configure via [GraphqliteBackend]: file (own connection per agent) or in-memory shared (one connection, serialized).
pub struct GraphqliteStoreBuilder {
    backend: Option<GraphqliteBackend>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

impl GraphqliteStoreBuilder {
    pub fn new() -> Self {
        Self {
            backend: None,
            mermaid_cache: None,
        }
    }

    /// File-backed store. Each [build] opens a new connection to the path; each agent gets its own connection. Concurrent.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self {
            backend: Some(GraphqliteBackend::file(path)),
            mermaid_cache: None,
        }
    }

    /// In-memory store shared by all callers. First [build] creates one connection and one worker; subsequent [build] returns a clone. Serialized access.
    pub fn in_memory() -> Self {
        Self {
            backend: Some(GraphqliteBackend::in_memory_shared()),
            mermaid_cache: None,
        }
    }

    /// Use an explicit backend (e.g. [GraphqliteBackend::file] or [GraphqliteBackend::in_memory_shared]).
    pub fn backend(backend: GraphqliteBackend) -> Self {
        Self {
            backend: Some(backend),
            mermaid_cache: None,
        }
    }

    /// Attach Mermaid cache for context-scoped invalidation on add_event. File-backed only.
    pub fn with_mermaid_cache(mut self, cache: Arc<MermaidCache>) -> Self {
        self.mermaid_cache = Some(cache);
        self
    }

    /// Build the store. File: new connection per call. In-memory shared: shared store, cloned.
    pub fn build(self) -> Result<Arc<GraphqliteProvenanceStore>> {
        let backend = self.backend.ok_or_else(|| ProvenanceError::InvalidEvent {
            event_id: String::new(),
            reason: "GraphqliteStoreBuilder: no backend set".to_string(),
        })?;
        backend.build_store(self.mermaid_cache)
    }

    /// Build a store that is **not** registered in the global file-store cache.
    ///
    /// The returned store owns its worker thread and SQLite connection exclusively;
    /// when all `Arc` references are dropped the worker exits and file descriptors
    /// are released. Use this in benchmarks and tests that create many short-lived
    /// stores to avoid leaking entries in the process-wide cache.
    pub fn build_uncached(self) -> Result<Arc<GraphqliteProvenanceStore>> {
        let backend = self.backend.ok_or_else(|| ProvenanceError::InvalidEvent {
            event_id: String::new(),
            reason: "GraphqliteStoreBuilder: no backend set".to_string(),
        })?;
        match backend {
            GraphqliteBackend::File(config) => build_store_from_config(&config),
            GraphqliteBackend::InMemoryShared => {
                build_store_from_config(&GraphqliteStoreConfig::in_memory())
            }
        }
    }
}

impl Default for GraphqliteStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{
        Outcome,
        ids::{AgentId, ArtifactId, EventId, ExternalId, MessageId, TaskId, UuidId},
    };
    use insta::{assert_json_snapshot, assert_snapshot};
    use serde_json::json;

    use super::*;
    use crate::{
        AgentType, CallScope, LlmUsage, ProvEvent, ProvEventData, TaskScopedEvent,
        graph_export::{
            GraphExporter, sequence::render_sequence_diagram, simplify::simplify_graph,
        },
    };

    /// Build a store backed by a unique temp path so tests can run concurrently.
    fn build_test_store() -> Arc<GraphqliteProvenanceStore> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("provenance.db");
        GraphqliteStoreBuilder::file(path)
            .build()
            .expect("build store")
    }

    /// Regression: GraphQLite does not apply plain SET after MERGE; ON CREATE SET / ON MATCH SET work.
    #[tokio::test]
    async fn graphqlite_on_create_match_set_required() {
        let store = build_test_store();
        store
            .run_cypher_execute(
                "MERGE (n:Message {id: 'test-msg-1'}) ON CREATE SET n.a2a_context_id = 'ctx-1-1' ON MATCH SET n.a2a_context_id = 'ctx-1-1'",
                &serde_json::Map::new(),
            )
            .await
            .expect("merge");
        let (query, params) = ConversationReadModel::message_query_storage_safe_params("ctx-1-1");
        let params = match &params {
            Value::Object(m) => m.clone(),
            _ => panic!("expected object"),
        };
        let results = store
            .run_cypher_for_test_with_params(&query, &params)
            .await
            .expect("match");
        assert!(
            !results.is_empty(),
            "ON CREATE/ON MATCH SET must persist props for MATCH to find"
        );
    }

    #[tokio::test]
    async fn graphqlite_message_query_returns_rows_and_columns() {
        let store = build_test_store();
        let context_id = ContextId::new(1, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(0),
                timestamp_ms: 1_700_000_000_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("test").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "test@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(1),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(2),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(3),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-1")),
                    role: "user".to_string(),
                    content: vec!["Hello".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];
        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let (query, params) =
            ConversationReadModel::message_query_storage_safe_params(context_id.as_str());
        let params = match params {
            Value::Object(map) => map,
            _ => panic!("expected object params"),
        };
        let results = store
            .run_cypher_for_test_with_params(&query, &params)
            .await
            .expect("run_cypher");
        assert!(
            !results.is_empty(),
            "expected at least one Message row; columns = {:?}",
            results.columns()
        );
    }

    /// Verifies that WAS_EXECUTED_BY edges are created and readable after add_event.
    #[tokio::test]
    async fn graphqlite_was_executed_by_edges_exist_after_message_sent() {
        let store = build_test_store();
        let context_id = ContextId::new(77, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-was-exec-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000077").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(700),
                timestamp_ms: 1_700_000_700_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("test").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "test@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(701),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(702),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(703),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_003,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-was-exec-1")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Test".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];
        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let query =
            "MATCH (a)-[r]->(b) WHERE type(r) = 'WAS_EXECUTED_BY' RETURN a.id AS src, b.id AS tgt";
        let results = store
            .run_cypher_for_test_with_params(query, &Map::new())
            .await
            .expect("run_cypher");
        assert!(
            !results.is_empty(),
            "expected at least one WAS_EXECUTED_BY edge; got {} rows",
            results.len()
        );
    }

    #[tokio::test]
    async fn graphqlite_context_messages_returns_two_after_message_sent() {
        let store = build_test_store();
        let context_id = ContextId::new(1, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(0),
                timestamp_ms: 1_700_000_000_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("test").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "test@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(1),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(2),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(2),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-1")),
                    role: "user".to_string(),
                    content: vec!["Hello".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(3),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_000_003,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-2")),
                    role: "assistant".to_string(),
                    content: vec!["Hi there.".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];
        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }
        let messages = store
            .context_messages(&context_id, None)
            .await
            .expect("context_messages");
        let snapshot: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                json!({
                    "message_id": m.message_id.as_str(),
                    "timestamp_ms": m.timestamp_ms,
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();
        assert_json_snapshot!(snapshot);

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(mermaid);
    }

    #[tokio::test]
    async fn graphqlite_sequence_rendering_covers_lifecycle_errors_and_multi_agent() {
        let store = build_test_store();
        let context_id = ContextId::new(7, 1);
        let planner_task_id = TaskId::from_external(ExternalId::new("task-planner-1"));
        let worker_task_id = TaskId::from_external(ExternalId::new("task-worker-1"));
        let planner_agent =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000101").unwrap());
        let worker_agent =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000102").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(10),
                timestamp_ms: 1_700_000_100_000,
                data: ProvEventData::AgentBooted {
                    agent_id: planner_agent.clone(),
                    agent_type: AgentType::new("planner_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "planner@1.0.0".to_string(),
                },
            }),
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(11),
                timestamp_ms: 1_700_000_100_001,
                data: ProvEventData::AgentBooted {
                    agent_id: worker_agent.clone(),
                    agent_type: AgentType::new("worker_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "worker@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(12),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_002,
                data: ProvEventData::TaskExists {
                    task_id: planner_task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(12),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_003,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: planner_task_id.clone(),
                    agent_id: planner_agent.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(13),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_003,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-1")),
                    role: "user".to_string(),
                    content: vec!["Plan and delegate work".to_string()],
                    metadata: None,
                    agent_id: planner_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(14),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_004,
                data: ProvEventData::TaskStatusChanged {
                    task_id: planner_task_id.clone(),
                    old_status: None,
                    new_status: Some("submitted".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(15),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_005,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: planner_task_id.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "PlannerStep".to_string(),
                    prompt: serde_json::json!({"messages":[{"role":"system","content":"plan"}]}),
                    metadata: serde_json::json!({
                        "agent_id": planner_agent.as_str(),
                        "task_id": planner_task_id.as_str(),
                        "message_id": "msg-user-1"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 3200,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(16),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_006,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: planner_task_id.clone(),
                    },
                    tool_name: "support/a2aRelay".to_string(),
                    function_name: None,
                    args: serde_json::json!({"target":"worker_agent","action":"delegate"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": planner_agent.as_str(),
                        "task_id": planner_task_id.as_str(),
                        "message_id":"msg-user-1",
                        "result":{"forwarded":true}
                    }),
                    duration_ms: 450,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(17),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_100_008,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-planner-1")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Delegated to worker".to_string()],
                    metadata: None,
                    agent_id: planner_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(18),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_009,
                data: ProvEventData::TaskExists {
                    task_id: worker_task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(18),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_010,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: worker_task_id.clone(),
                    agent_id: worker_agent.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(19),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_010,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-worker-internal-1")),
                    role: "user".to_string(),
                    content: vec!["Perform delegated action".to_string()],
                    metadata: None,
                    agent_id: worker_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(20),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_011,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: worker_task_id.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "WorkerStep".to_string(),
                    prompt: serde_json::json!({"messages":[{"role":"system","content":"execute"}]}),
                    metadata: serde_json::json!({
                        "agent_id": worker_agent.as_str(),
                        "task_id": worker_task_id.as_str(),
                        "message_id": "msg-worker-internal-1",
                        "error":"rate limited"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 2100,
                    outcome: Outcome::Failure,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(21),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_012,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: worker_task_id.clone(),
                    },
                    tool_name: "support/clickup".to_string(),
                    function_name: None,
                    args: serde_json::json!({"action":"CreateTask","name":"demo"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": worker_agent.as_str(),
                        "task_id": worker_task_id.as_str(),
                        "message_id":"msg-worker-internal-1",
                        "error":"permission denied"
                    }),
                    duration_ms: 600,
                    outcome: Outcome::Failure,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(22),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_100_013,
                data: ProvEventData::TaskStatusChanged {
                    task_id: worker_task_id.clone(),
                    old_status: Some("working".to_string()),
                    new_status: Some("failed".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(23),
                context_id: context_id.clone(),
                task_id: worker_task_id,
                timestamp_ms: 1_700_000_100_014,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-worker-1")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Worker failed: permission denied".to_string()],
                    metadata: None,
                    agent_id: worker_agent.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_covers_lifecycle_errors_and_multi_agent_mermaid",
            mermaid
        );
    }

    #[tokio::test]
    async fn graphqlite_sequence_rendering_documents_tool_failure_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(8, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-tool-failure-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000111").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(200),
                timestamp_ms: 1_700_000_200_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("ops_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "ops@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(201),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_200_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(201),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_200_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(202),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_200_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-tool-fail")),
                    role: "user".to_string(),
                    content: vec!["Delete stale objects".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(203),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_200_003,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    tool_name: "support/storage".to_string(),
                    function_name: None,
                    args: serde_json::json!({"action":"Delete","bucket":"prod-artifacts"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id":"msg-user-tool-fail",
                        "error":"permission denied"
                    }),
                    duration_ms: 780,
                    outcome: Outcome::Failure,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(204),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_200_005,
                data: ProvEventData::TaskStatusChanged {
                    task_id: task_id.clone(),
                    old_status: Some("working".to_string()),
                    new_status: Some("failed".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(205),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_200_006,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-agent-tool-fail")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Storage delete failed: permission denied".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_documents_tool_failure_mermaid",
            mermaid
        );
    }

    #[tokio::test]
    async fn graphqlite_sequence_rendering_documents_baml_rejection_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(9, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-baml-reject-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000112").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(300),
                timestamp_ms: 1_700_000_300_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("planner_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "planner@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(301),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_300_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(301),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_300_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(302),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_300_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-baml-fail")),
                    role: "user".to_string(),
                    content: vec!["Produce structured plan output".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(303),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_300_003,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "PlannerStep".to_string(),
                    prompt: serde_json::json!({"messages":[{"role":"system","content":"plan"}]}),
                    metadata: serde_json::json!({
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id":"msg-user-baml-fail"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 1800,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(304),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_300_004,
                data: ProvEventData::PromptRejected {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    llm_call_event_id: EventId::from_counter(303),
                    reason: "BAML validation failed: missing field plan_steps".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(305),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_300_005,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-agent-baml-fail")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Model output rejected by BAML validator.".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_documents_baml_rejection_mermaid",
            mermaid
        );
    }

    #[tokio::test]
    async fn graphqlite_sequence_rendering_documents_cross_agent_return_handoff_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(10, 1);
        let planner_task_id = TaskId::from_external(ExternalId::new("task-planner-handoff-1"));
        let worker_task_id = TaskId::from_external(ExternalId::new("task-worker-handoff-1"));
        let planner_agent =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000121").unwrap());
        let worker_agent =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000122").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(400),
                timestamp_ms: 1_700_000_400_000,
                data: ProvEventData::AgentBooted {
                    agent_id: planner_agent.clone(),
                    agent_type: AgentType::new("planner_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "planner@1.0.0".to_string(),
                },
            }),
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(401),
                timestamp_ms: 1_700_000_400_001,
                data: ProvEventData::AgentBooted {
                    agent_id: worker_agent.clone(),
                    agent_type: AgentType::new("worker_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "worker@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(402),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_400_002,
                data: ProvEventData::TaskExists {
                    task_id: planner_task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(402),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_400_003,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: planner_task_id.clone(),
                    agent_id: planner_agent.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(403),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_400_003,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-handoff-1")),
                    role: "user".to_string(),
                    content: vec!["Coordinate with worker and return summary".to_string()],
                    metadata: None,
                    agent_id: planner_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(404),
                context_id: context_id.clone(),
                task_id: planner_task_id.clone(),
                timestamp_ms: 1_700_000_400_004,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: planner_task_id.clone(),
                    },
                    tool_name: "support/a2aRelay".to_string(),
                    function_name: None,
                    args: serde_json::json!({"target":"worker_agent","action":"prepare_report"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": planner_agent.as_str(),
                        "task_id": planner_task_id.as_str(),
                        "message_id":"msg-user-handoff-1",
                        "result":{"forwarded":true}
                    }),
                    duration_ms: 120,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(405),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_400_005,
                data: ProvEventData::TaskExists {
                    task_id: worker_task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(405),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_400_006,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: worker_task_id.clone(),
                    agent_id: worker_agent.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(406),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_400_006,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-worker-in-1")),
                    role: "user".to_string(),
                    content: vec!["prepare_report".to_string()],
                    metadata: None,
                    agent_id: worker_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(407),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_400_007,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: worker_task_id.clone(),
                    },
                    tool_name: "support/clickup".to_string(),
                    function_name: None,
                    args: serde_json::json!({"action":"ListTasks","list_id":"abc"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": worker_agent.as_str(),
                        "task_id": worker_task_id.as_str(),
                        "message_id":"msg-worker-in-1",
                        "result":{"count":3}
                    }),
                    duration_ms: 250,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(408),
                context_id: context_id.clone(),
                task_id: worker_task_id.clone(),
                timestamp_ms: 1_700_000_400_008,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-worker-out-1")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Worker summary ready (3 items).".to_string()],
                    metadata: None,
                    agent_id: worker_agent.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(409),
                context_id: context_id.clone(),
                task_id: planner_task_id,
                timestamp_ms: 1_700_000_400_009,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-planner-out-1")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Final summary from worker: 3 items.".to_string()],
                    metadata: None,
                    agent_id: planner_agent.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");

        // Assert "Final summary" message is attributed to planner in the graph (WAS_EXECUTED_BY).
        let final_msg_id = exported
            .nodes
            .iter()
            .filter(|n| n.label == GraphNodeLabel::Message.as_str())
            .find(|n| {
                let c = n.properties.get(crate::vocabulary::a2a::CONTENT);
                let s = c.and_then(|v| v.as_str()).or_else(|| {
                    c.and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                });
                s.is_some_and(|s| s.contains("Final summary from worker"))
            })
            .map(|n| n.id.as_str())
            .expect("Final summary message node");
        let mp_id = exported
            .edges
            .iter()
            .find(|e| {
                e.from == final_msg_id && e.relation == crate::graph_model::EDGE_WAS_EMITTED_BY
            })
            .map(|e| e.to.as_str())
            .expect("MessageProcessing for Final summary");
        let executing_agent = exported
            .edges
            .iter()
            .find(|e| e.from == mp_id && e.relation == crate::graph_model::EDGE_WAS_EXECUTED_BY)
            .map(|e| e.to.as_str())
            .expect("WAS_EXECUTED_BY agent for Final summary MP");
        let planner_instance_id = format!("agent_instance:{}", planner_agent.as_str());
        assert_eq!(
            executing_agent, planner_instance_id,
            "Final summary should be attributed to planner (task_id was planner task)"
        );

        let simplified = simplify_graph(&exported);
        // Re-assert on simplified graph: agent for "Final summary" should still be planner.
        let final_msg_id_simp = simplified
            .nodes
            .iter()
            .filter(|n| n.label == GraphNodeLabel::Message.as_str())
            .find(|n| {
                let c = n.properties.get(crate::vocabulary::a2a::CONTENT);
                let s = c.and_then(|v| v.as_str()).or_else(|| {
                    c.and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                });
                s.is_some_and(|s| s.contains("Final summary from worker"))
            })
            .map(|n| n.id.as_str())
            .expect("Final summary message in simplified");
        let mp_id_simp = simplified
            .edges
            .iter()
            .find(|e| {
                e.from == final_msg_id_simp && e.relation == crate::graph_model::EDGE_WAS_EMITTED_BY
            })
            .map(|e| e.to.as_str())
            .expect("MP for Final summary in simplified");
        let exec_agent_simp = simplified
            .edges
            .iter()
            .find(|e| {
                e.from == mp_id_simp && e.relation == crate::graph_model::EDGE_WAS_EXECUTED_BY
            })
            .map(|e| e.to.as_str());
        assert_eq!(
            exec_agent_simp,
            Some(planner_instance_id.as_str()),
            "simplified graph: Final summary MP should have WAS_EXECUTED_BY to planner, got {:?}",
            exec_agent_simp
        );

        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_documents_cross_agent_return_handoff_mermaid",
            mermaid
        );
    }

    /// Interleaved parallel tasks in one context: task A and task B events
    /// ordered by timestamp so diagram stresses rect grouping and ordering.
    #[tokio::test]
    async fn graphqlite_sequence_rendering_interleaved_parallel_tasks_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(11, 1);
        let task_a = TaskId::from_external(ExternalId::new("task-interleave-a"));
        let task_b = TaskId::from_external(ExternalId::new("task-interleave-b"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000131").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(500),
                timestamp_ms: 1_700_000_500_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("runner_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "runner@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(501),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_001,
                data: ProvEventData::TaskExists {
                    task_id: task_a.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(501),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_a.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(503),
                context_id: context_id.clone(),
                task_id: task_b.clone(),
                timestamp_ms: 1_700_000_500_002,
                data: ProvEventData::TaskExists {
                    task_id: task_b.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(503),
                context_id: context_id.clone(),
                task_id: task_b.clone(),
                timestamp_ms: 1_700_000_500_003,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_b.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(505),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_003,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-a-in")),
                    role: "user".to_string(),
                    content: vec!["Run A".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(504),
                context_id: context_id.clone(),
                task_id: task_b.clone(),
                timestamp_ms: 1_700_000_500_004,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-b-in")),
                    role: "user".to_string(),
                    content: vec!["Run B".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(505),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_005,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_a.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "StepA".to_string(),
                    prompt: serde_json::json!({"messages":[]}),
                    metadata: serde_json::json!({
                        "agent_id": agent_id.as_str(),
                        "task_id": task_a.as_str(),
                        "message_id": "msg-a-in"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 100,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(506),
                context_id: context_id.clone(),
                task_id: task_b.clone(),
                timestamp_ms: 1_700_000_500_006,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_b.clone(),
                    },
                    tool_name: "support/weather".to_string(),
                    function_name: None,
                    args: serde_json::json!({"city":"NYC"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": agent_id.as_str(),
                        "task_id": task_b.as_str(),
                        "message_id": "msg-b-in",
                        "result":{"temp":72}
                    }),
                    duration_ms: 50,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(507),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_007,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_a.clone(),
                    },
                    tool_name: "support/calculator".to_string(),
                    function_name: None,
                    args: serde_json::json!({"op":"add","a":1,"b":2}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": agent_id.as_str(),
                        "task_id": task_a.as_str(),
                        "message_id": "msg-a-in",
                        "result":3
                    }),
                    duration_ms: 20,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(508),
                context_id: context_id.clone(),
                task_id: task_a.clone(),
                timestamp_ms: 1_700_000_500_008,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-a-out")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["A done (3)".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(509),
                context_id: context_id.clone(),
                task_id: task_b.clone(),
                timestamp_ms: 1_700_000_500_009,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-b-out")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["B done (72°F)".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_interleaved_parallel_tasks_mermaid",
            mermaid
        );
    }

    /// Rejection + recovery: PromptRejected followed by successful LLM retry.
    #[tokio::test]
    async fn graphqlite_sequence_rendering_rejection_recovery_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(12, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-reject-retry-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000132").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(600),
                timestamp_ms: 1_700_000_600_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("validator_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "validator@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(601),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(601),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(602),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-retry")),
                    role: "user".to_string(),
                    content: vec!["Return valid JSON".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(603),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_003,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "FirstAttempt".to_string(),
                    prompt: serde_json::json!({"messages":[]}),
                    metadata: serde_json::json!({
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id": "msg-user-retry"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 800,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(604),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_004,
                data: ProvEventData::PromptRejected {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    llm_call_event_id: EventId::from_counter(603),
                    reason: "invalid schema: missing required field".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(605),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_600_005,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    client: "DefaultClient".to_string(),
                    model: "openai-generic".to_string(),
                    function_name: "RetryAttempt".to_string(),
                    prompt: serde_json::json!({"messages":[]}),
                    metadata: serde_json::json!({
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id": "msg-user-retry"
                    }),
                    usage: LlmUsage::Unknown,
                    duration_ms: 600,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(606),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_600_006,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-agent-retry")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Valid JSON after retry.".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_rejection_recovery_mermaid",
            mermaid
        );
    }

    /// Multi-tool chain: first tool success, second tool failure, terminal synthesis message.
    #[tokio::test]
    async fn graphqlite_sequence_rendering_multi_tool_mixed_outcomes_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(13, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-multi-tool-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000133").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(700),
                timestamp_ms: 1_700_000_700_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("chain_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "chain@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(701),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(701),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(702),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-chain")),
                    role: "user".to_string(),
                    content: vec!["Fetch then mutate".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(703),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_003,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    tool_name: "support/weather".to_string(),
                    function_name: None,
                    args: serde_json::json!({"city":"LA"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id": "msg-user-chain",
                        "result":{"temp":68}
                    }),
                    duration_ms: 100,
                    outcome: Outcome::Success,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(704),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_700_004,
                data: ProvEventData::ToolCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    tool_name: "support/clickup".to_string(),
                    function_name: None,
                    args: serde_json::json!({"action":"CreateTask","name":"from-chain"}),
                    metadata: serde_json::json!({
                        "phase":"send",
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str(),
                        "message_id": "msg-user-chain",
                        "error":"API rate limit"
                    }),
                    duration_ms: 400,
                    outcome: Outcome::Failure,
                    delegation_target: None,
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(705),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_700_005,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-agent-chain")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Weather 68°F; CreateTask failed: API rate limit".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_multi_tool_mixed_outcomes_mermaid",
            mermaid
        );
    }

    /// Status-only transitions and artifact-heavy completion (no LLM/tool in between).
    #[tokio::test]
    async fn graphqlite_sequence_rendering_status_and_artifacts_mermaid() {
        let store = build_test_store();
        let context_id = ContextId::new(14, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-status-artifacts-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000134").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(800),
                timestamp_ms: 1_700_000_800_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("report_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "report@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(801),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(801),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(802),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-user-report")),
                    role: "user".to_string(),
                    content: vec!["Generate report".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(803),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_003,
                data: ProvEventData::TaskStatusChanged {
                    task_id: task_id.clone(),
                    old_status: None,
                    new_status: Some("submitted".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(804),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_004,
                data: ProvEventData::TaskStatusChanged {
                    task_id: task_id.clone(),
                    old_status: Some("submitted".to_string()),
                    new_status: Some("working".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(805),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_005,
                data: ProvEventData::TaskArtifactGenerated {
                    task_id: task_id.clone(),
                    artifact_id: Some(ArtifactId::from_external(ExternalId::new("art-report-1"))),
                    artifact_type: Some("report".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(806),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_006,
                data: ProvEventData::TaskArtifactGenerated {
                    task_id: task_id.clone(),
                    artifact_id: Some(ArtifactId::from_external(ExternalId::new("art-chart-1"))),
                    artifact_type: Some("chart".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(807),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_800_007,
                data: ProvEventData::TaskStatusChanged {
                    task_id: task_id.clone(),
                    old_status: Some("working".to_string()),
                    new_status: Some("completed".to_string()),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(808),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_800_008,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-agent-report")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Report and chart ready.".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let exported = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified = simplify_graph(&exported);
        let mermaid = render_sequence_diagram(&simplified);
        assert_snapshot!(
            "graphqlite_sequence_rendering_status_and_artifacts_mermaid",
            mermaid
        );
    }

    /// Determinism guard: same context exported and rendered twice yields identical Mermaid.
    /// Scenario uses explicit event IDs (from_counter) and timestamps so ordering is stable.
    #[tokio::test]
    async fn graphqlite_sequence_rendering_deterministic_for_fixed_events() {
        let store = build_test_store();
        let context_id = ContextId::new(15, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-determinism-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000135").unwrap());

        let events = [
            ProvEvent::AgentBooted(AgentBootedEvent {
                id: EventId::from_counter(900),
                timestamp_ms: 1_700_000_900_000,
                data: ProvEventData::AgentBooted {
                    agent_id: agent_id.clone(),
                    agent_type: AgentType::new("det_agent").expect("agent_type"),
                    agent_version: "1.0.0".to_string(),
                    archive_path: "det@1.0.0".to_string(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(901),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_900_001,
                data: ProvEventData::TaskExists {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(901),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_900_002,
                data: ProvEventData::TaskExecutionStarted {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    context_id: context_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(902),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1_700_000_900_002,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from_external(ExternalId::new("msg-det-in")),
                    role: "user".to_string(),
                    content: vec!["Stable input".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
            ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(903),
                context_id: context_id.clone(),
                task_id,
                timestamp_ms: 1_700_000_900_003,
                data: ProvEventData::MessageSent {
                    id: MessageId::from_external(ExternalId::new("msg-det-out")),
                    role: "ROLE_AGENT".to_string(),
                    content: vec!["Stable output".to_string()],
                    metadata: None,
                    agent_id: agent_id.clone(),
                },
            }),
        ];

        for event in &events {
            store.add_event(event.clone()).await.expect("add_event");
        }

        let export_once = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified_once = simplify_graph(&export_once);
        let mermaid_once = render_sequence_diagram(&simplified_once);

        let export_twice = GraphExporter::new(store.clone())
            .export_by_context(context_id.as_str())
            .await
            .expect("export graph by context");
        let simplified_twice = simplify_graph(&export_twice);
        let mermaid_twice = render_sequence_diagram(&simplified_twice);

        assert_eq!(
            mermaid_once, mermaid_twice,
            "sequence diagram must be deterministic for fixed event IDs and timestamps"
        );
    }
}
