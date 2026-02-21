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
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::Instant,
};

use async_trait::async_trait;
use baml_rt_core::ids::ContextId;
use graphqlite::{Connection, CypherResult, Graph, Row};
use serde_json::{Map, Value};
use tokio::sync::{Mutex as TokioMutex, oneshot};

use crate::{
    cypher_build::{self, KeyStyle},
    error::{ProvenanceError, Result},
    graph_model::{ConversationReadModel, TOOL_CALL_ARGS_EDGE},
    graphqlite_config::GraphqliteStoreConfig,
    normalizer::{DefaultProvNormalizer, ProvNormalizer, validate_event},
    spans,
    store::{
        ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
        ProvenanceQueryApi, ProvenanceWriter, ToolSessionPhase,
    },
    vocabulary::message_directions,
};

// Column names from ConversationReadModel RETURN clauses (storage-safe underscore form).
const MSG_COL_EVENT_ID: &str = "m.a2a_event_id";
const MSG_COL_MESSAGE_ID: &str = "m.a2a_message_id";
const MSG_COL_DIRECTION: &str = "m.a2a_direction";
const MSG_COL_ROLE: &str = "m.a2a_role";
const MSG_COL_CONTENT: &str = "m.a2a_content";
const MSG_COL_EVENT_ID_ALT: &str = "a2a_event_id";
const MSG_COL_MESSAGE_ID_ALT: &str = "a2a_message_id";
const MSG_COL_DIRECTION_ALT: &str = "a2a_direction";
const MSG_COL_ROLE_ALT: &str = "a2a_role";
const MSG_COL_CONTENT_ALT: &str = "a2a_content";

const TOOL_COL_EVENT_ID: &str = "t.a2a_event_id";
const TOOL_COL_TOOL_NAME: &str = "t.a2a_tool_name";
const TOOL_COL_METADATA: &str = "toString(t.a2a_metadata)";
const TOOL_COL_ARGS: &str = "toString(args.a2a_args)";
const TOOL_COL_SUCCESS: &str = "t.a2a_success";
const TOOL_COL_EVENT_ID_ALT: &str = "t.`a2a:event_id`";
const TOOL_COL_TOOL_NAME_ALT: &str = "t.`a2a:tool_name`";
const TOOL_COL_METADATA_ALT: &str = "toString(t.`a2a:metadata`)";
const TOOL_COL_ARGS_ALT: &str = "toString(args.`a2a:args`)";
const TOOL_COL_ROLE: &str = "used.prov_role";
const TOOL_COL_ROLE_ALT: &str = "used.`prov:role`";
const TOOL_COL_TARGET_TYPE: &str = "args.prov_type";
const TOOL_COL_TARGET_TYPE_ALT: &str = "args.`prov:type`";
const TOOL_COL_SUCCESS_ALT: &str = "t.`a2a:success`";

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
}

/// Provenance-only store backed by GraphQLite (SQLite + Cypher).
/// A worker thread owns the connection; the store is Send + Sync via channel.
pub struct GraphqliteProvenanceStore {
    request_tx: mpsc::SyncSender<WorkerRequest>,
    normalizer: Arc<dyn ProvNormalizer>,
}

/// Strong-typed message row from GraphQLite result. No parsing; use Row::get.
struct MessageRow {
    event_id: String,
    message_id: String,
    direction: String,
    role: String,
    content: Value,
}

impl MessageRow {
    fn from_row(row: &Row) -> std::result::Result<Self, graphqlite::Error> {
        let event_id: String = row
            .get(MSG_COL_EVENT_ID)
            .or_else(|_| row.get(MSG_COL_EVENT_ID_ALT))?;
        let message_id: String = row
            .get(MSG_COL_MESSAGE_ID)
            .or_else(|_| row.get(MSG_COL_MESSAGE_ID_ALT))?;
        let direction: String = row
            .get(MSG_COL_DIRECTION)
            .or_else(|_| row.get(MSG_COL_DIRECTION_ALT))?;
        let role: String = row
            .get(MSG_COL_ROLE)
            .or_else(|_| row.get(MSG_COL_ROLE_ALT))?;
        let content_str: String = row
            .get(MSG_COL_CONTENT)
            .or_else(|_| row.get(MSG_COL_CONTENT_ALT))?;
        let content = serde_json::from_str(&content_str).unwrap_or(Value::String(content_str));
        Ok(Self {
            event_id,
            message_id,
            direction,
            role,
            content,
        })
    }
}

/// Strong-typed tool-call row from GraphQLite result.
struct ToolCallRow {
    event_id: String,
    tool_name: String,
    metadata: Value,
    args: Value,
    role: String,
    target_type: String,
    success: Option<bool>,
}

fn parse_bool_string(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" => Some(true),
        "false" | "f" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn decode_optional_bool(row: &Row, primary_col: &str, alt_col: &str) -> Option<bool> {
    if let Ok(v) = row
        .get::<bool>(primary_col)
        .or_else(|_| row.get::<bool>(alt_col))
    {
        return Some(v);
    }
    if let Ok(v) = row
        .get::<i64>(primary_col)
        .or_else(|_| row.get::<i64>(alt_col))
    {
        return Some(v != 0);
    }
    if let Ok(raw) = row
        .get::<String>(primary_col)
        .or_else(|_| row.get::<String>(alt_col))
    {
        if let Some(parsed) = parse_bool_string(&raw) {
            return Some(parsed);
        }
        tracing::debug!(
            column = %primary_col,
            alt_column = %alt_col,
            value = %raw,
            "unable to parse optional bool field from string"
        );
    }
    None
}

impl ToolCallRow {
    fn from_row(row: &Row) -> std::result::Result<Self, graphqlite::Error> {
        let event_id: String = row
            .get(TOOL_COL_EVENT_ID)
            .or_else(|_| row.get(TOOL_COL_EVENT_ID_ALT))?;
        let tool_name: String = row
            .get(TOOL_COL_TOOL_NAME)
            .or_else(|_| row.get(TOOL_COL_TOOL_NAME_ALT))?;
        let metadata_str: String = row
            .get(TOOL_COL_METADATA)
            .or_else(|_| row.get(TOOL_COL_METADATA_ALT))?;
        let args_str: String = row
            .get(TOOL_COL_ARGS)
            .or_else(|_| row.get(TOOL_COL_ARGS_ALT))?;
        let metadata = serde_json::from_str(&metadata_str).unwrap_or(Value::String(metadata_str));
        let args = serde_json::from_str(&args_str).unwrap_or(Value::String(args_str));
        let role: String = row
            .get(TOOL_COL_ROLE)
            .or_else(|_| row.get(TOOL_COL_ROLE_ALT))
            .unwrap_or_default();
        let target_type: String = row
            .get(TOOL_COL_TARGET_TYPE)
            .or_else(|_| row.get(TOOL_COL_TARGET_TYPE_ALT))
            .unwrap_or_default();
        let success = decode_optional_bool(row, TOOL_COL_SUCCESS, TOOL_COL_SUCCESS_ALT);
        Ok(Self {
            event_id,
            tool_name,
            metadata,
            args,
            role,
            target_type,
            success,
        })
    }

    fn is_completed(&self) -> bool {
        self.success.is_some()
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

impl GraphqliteProvenanceStore {
    /// Open a graph from config and enable WAL if requested.
    fn open_graph(config: &GraphqliteStoreConfig) -> std::result::Result<Graph, graphqlite::Error> {
        let conn = match &config.path {
            crate::graphqlite_config::StorePath::InMemory => Connection::open_in_memory()?,
            crate::graphqlite_config::StorePath::File(path) => Connection::open(path.as_path())?,
        };
        if config.wal {
            conn.sqlite_connection()
                .execute_batch("PRAGMA journal_mode=WAL")
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
    pub fn build_store(&self) -> Result<Arc<GraphqliteProvenanceStore>> {
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
                get_or_init_file_store(path, config.clone())
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
            }
        }
    });
    let store = GraphqliteProvenanceStore {
        request_tx,
        normalizer: Arc::new(DefaultProvNormalizer::default()),
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
}

#[async_trait]
impl ProvenanceWriter for GraphqliteProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        let _start = Instant::now();
        validate_event(&event)?;
        let normalized = self.normalizer.normalize(&event)?;
        let statements = cypher_build::build_queries_with_key_style_params(
            &normalized,
            KeyStyle::StorageSafeUnderscore,
        );
        for stmt in &statements {
            let params = require_object_params(&stmt.params)?;
            self.run_cypher_write(&stmt.query, &params).await?;
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
        let results = self.run_cypher_with_params(query, &params).await?;
        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in results.iter() {
            let msg = match MessageRow::from_row(row) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let role = if msg.direction == message_directions::RECEIVED {
                "user".to_string()
            } else {
                msg.role
            };
            let content = normalize_message_content(&msg.content);
            if content.trim().is_empty() {
                continue;
            }
            messages.push(ProvenanceContextMessage {
                message_id: msg.message_id,
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
            .run_cypher_with_params(message_query, &message_params)
            .await?;
        let tool_results = self
            .run_cypher_with_params(tool_query, &tool_params)
            .await?;

        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in message_results.iter() {
            let msg = match MessageRow::from_row(row) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let role = if msg.direction == message_directions::RECEIVED {
                "user".to_string()
            } else {
                msg.role
            };
            let content = normalize_message_content(&msg.content);
            if content.trim().is_empty() {
                continue;
            }
            items.push(ProvenanceConversationContextItem {
                timestamp_ms: event_id_to_timestamp_ms(&msg.event_id),
                event_id: msg.event_id,
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
            let phase = ToolSessionPhase::from_metadata(&tool.metadata);
            let result = tool
                .metadata
                .get("result")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let error = metadata_error(&tool.metadata);
            let has_outcome = has_meaningful_result(&result) || error.is_some();
            let include_call = !matches!(
                phase,
                ToolSessionPhase::Open | ToolSessionPhase::Finish | ToolSessionPhase::Abort
            ) && (!is_empty_object(&tool.args) || has_outcome);

            if include_call {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: event_id_to_timestamp_ms(&tool.event_id),
                    event_id: tool.event_id.clone(),
                    role: "assistant".to_string(),
                    content: serde_json::json!({
                        "tool_call": {
                            "name": tool.tool_name,
                            "args": tool.args,
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
                    event_id: tool.event_id,
                    role: "tool".to_string(),
                    content: Value::Object(content),
                    source: "tool_result".to_string(),
                });
            }
        }

        items.sort_by_key(|i| {
            (
                i.timestamp_ms,
                event_id_to_timestamp_ms(&i.event_id),
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

/// Builder for the GraphQLite provenance store. Configure via [GraphqliteBackend]: file (own connection per agent) or in-memory shared (one connection, serialized).
pub struct GraphqliteStoreBuilder {
    backend: Option<GraphqliteBackend>,
}

impl GraphqliteStoreBuilder {
    pub fn new() -> Self {
        Self { backend: None }
    }

    /// File-backed store. Each [build] opens a new connection to the path; each agent gets its own connection. Concurrent.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self {
            backend: Some(GraphqliteBackend::file(path)),
        }
    }

    /// In-memory store shared by all callers. First [build] creates one connection and one worker; subsequent [build] returns a clone. Serialized access.
    pub fn in_memory() -> Self {
        Self {
            backend: Some(GraphqliteBackend::in_memory_shared()),
        }
    }

    /// Use an explicit backend (e.g. [GraphqliteBackend::file] or [GraphqliteBackend::in_memory_shared]).
    pub fn backend(backend: GraphqliteBackend) -> Self {
        Self {
            backend: Some(backend),
        }
    }

    /// Build the store. File: new connection per call. In-memory shared: shared store, cloned.
    pub fn build(self) -> Result<Arc<GraphqliteProvenanceStore>> {
        let backend = self.backend.ok_or_else(|| ProvenanceError::InvalidEvent {
            event_id: String::new(),
            reason: "GraphqliteStoreBuilder: no backend set".to_string(),
        })?;
        backend.build_store()
    }
}

impl Default for GraphqliteStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{AgentId, EventId, ExternalId, MessageId, TaskId, UuidId};

    use super::*;
    use crate::{AgentType, GlobalEvent, ProvEvent, ProvEventData, TaskScopedEvent};

    /// Build a store backed by a unique temp path so tests can run concurrently.
    fn build_test_store() -> Arc<GraphqliteProvenanceStore> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep().join("provenance.db");
        GraphqliteStoreBuilder::file(path)
            .build()
            .expect("build store")
    }

    #[tokio::test]
    async fn graphqlite_message_query_returns_rows_and_columns() {
        let store = build_test_store();
        let context_id = ContextId::new(1, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

        let events = [
            ProvEvent::Global(GlobalEvent {
                id: EventId::from_counter(0),
                context_id: context_id.clone(),
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
                data: ProvEventData::TaskCreated {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
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
            .run_cypher_for_test_with_params(query, &params)
            .await
            .expect("run_cypher");
        assert!(
            !results.is_empty(),
            "expected at least one Message row; columns = {:?}",
            results.columns()
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
            ProvEvent::Global(GlobalEvent {
                id: EventId::from_counter(0),
                context_id: context_id.clone(),
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
                data: ProvEventData::TaskCreated {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
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
        assert_eq!(messages.len(), 2, "expect user + assistant message");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }
}
