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
//!
//! ## Query patterns
//!
//! **Context scoping:** Use `SCOPED_TO` edge traversal, not `WHERE props.a2a_context_id`.
//! ```sql
//! SELECT ... FROM prov_node WHERE node_id IN (
//!   SELECT VALUE from_id FROM prov_edge WHERE to_id = $ctx_node AND rel_type = 'SCOPED_TO' AND from_label = $label
//! )
//! ```
//!
//! **Task scoping:** Use `A2A_TASK_CALL` edge from `TaskExecution` activity node.
//! ```sql
//! SELECT ... FROM prov_node WHERE node_id IN (
//!   SELECT VALUE to_id FROM prov_edge WHERE from_id = $task_exec_node AND rel_type = 'A2A_TASK_CALL'
//! )
//! ```
//!
//! **Plan step identity:** Use deterministic `node_id` via `plan_step_entity_id_string(task, plan, step)`.
//! ```sql
//! SELECT ... FROM prov_node WHERE node_id = $step_node_id
//! ```
//!
//! **Payload reads:** Use deterministic `payload_id` via [`crate::payload_id::payload_row_id`]
//! (`payload:{anchor}:{kind}`), not `WHERE activity_anchor_id = $anchor AND payload_kind = $kind`.
//!
//! **Never** filter nodes by `props.a2a_context_id`, `props.a2a_task_id`, or other
//! relational-crutch properties in query WHERE clauses. These properties exist as
//! informational attributes for display/audit only; relationships are expressed as edges.

mod a2a_store;
mod archive_ref;
mod builder;
mod context_reader;
mod conversation_context_pipeline;
mod helpers;
mod ops_query;
mod payload;
mod planning_query;
mod schema;
mod writer;

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use baml_rt_core::{
    backoff::backoff_delay,
    ids::{AgentId, ContextId, TaskId},
};
pub use builder::{RemoteConfig, RemoteCredentials, SurrealBackend, SurrealStoreBuilder};
use dashmap::DashMap;
pub(crate) use helpers::{check_and_take_zero, map_surreal_error};
use serde_json::Value;
use surrealdb::{Surreal, engine::any::Any};

use crate::{
    error::{ProvenanceError, Result},
    id_semantics::{MessageEntityId, MessageEntityInput},
    mermaid_cache::MermaidCache,
    normalizer::ProvNormalizer,
    surreal_tables::{TBL_EDGE, TBL_NODE},
    types::ProvEntityId,
    vocabulary::context_scope,
};

/// Maximum number of attempts (including the first) for a write that hits
/// SurrealDB MVCC `Transaction conflict`. Concurrent writers targeting shared
/// records (context/agent nodes) need a small retry budget; provenance writes
/// are idempotent UPSERTs so retry is safe.
///
/// **Tuning signal:** if production observability ever shows a sustained
/// non-zero retry rate, the strategic fix is per-shared-record write coalescing
/// (single-flight on `agent_runtime_instance` / context entity per process) —
/// not raising this constant. See `record_event_required` callers and the
/// `concurrent_writes_test` regression coverage.
pub(super) const WRITE_CONFLICT_MAX_ATTEMPTS: u32 = 6;
const WRITE_CONFLICT_BASE_DELAY: Duration = Duration::from_millis(2);
const WRITE_CONFLICT_MAX_DELAY: Duration = Duration::from_millis(200);

/// True when `e` is the SurrealDB optimistic-concurrency conflict signal.
/// The error type carries no structured discriminator on the public boundary,
/// so we match on the message prefix (`Transaction conflict: …. This transaction
/// can be retried`, see `surrealdb-core`/`kvs/err.rs`).
pub(super) fn is_transaction_conflict(e: &surrealdb::Error) -> bool {
    e.message().contains("Transaction conflict")
}

/// True when `e` is SurrealDB's "record / unique index already exists" signal.
///
/// Used by `archive_ref.rs` (`archive_local_counter`): concurrent first-touch runs `UPDATE` then
/// `CREATE`; losers of the `CREATE` race must retry `UPDATE` instead of surfacing `RecordExists` /
/// `IndexExists` as a hard error.
pub(super) fn is_duplicate_record_write(e: &surrealdb::Error) -> bool {
    let m = e.message();
    (m.contains("Database record") && m.contains("already exists"))
        || (m.contains("Database index") && m.contains("already contains"))
}

/// True when `e` wraps a Surreal duplicate-record error (e.g. unique `activity_anchor`).
pub(super) fn storage_err_is_duplicate_record_write(e: &ProvenanceError) -> bool {
    match e {
        ProvenanceError::Storage(boxed) => boxed
            .downcast_ref::<surrealdb::Error>()
            .is_some_and(is_duplicate_record_write),
        _ => false,
    }
}

/// Equal-jitter backoff: half the budget is fixed, half is randomised in
/// `[0, base/2]`. Without jitter, N parallel retriers wake in lockstep and
/// re-collide on the next attempt; equal-jitter spreads them out without
/// lengthening the worst-case wait beyond the base delay.
///
/// Jitter source is `SystemTime::subsec_nanos` — decorrelation only, no crypto
/// guarantees needed and no extra dependency.
pub(super) fn jittered_backoff(attempt: u32) -> Duration {
    let base =
        backoff_delay(WRITE_CONFLICT_BASE_DELAY, WRITE_CONFLICT_MAX_DELAY, attempt).as_nanos();
    let half = (base / 2) as u64;
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_nanos = if half > 0 { jitter % half } else { 0 };
    Duration::from_nanos(half + jitter_nanos)
}

/// SurrealDB-backed provenance store — the canonical implementation of all provenance traits.
pub struct SurrealProvenanceStore {
    db: Surreal<Any>,
    normalizer: Arc<dyn ProvNormalizer>,
    mermaid_cache: Option<Arc<MermaidCache>>,
    /// In-process cache of successful task → agent resolution for [`SurrealProvenanceStore::get_task_agent_id`].
    ///
    /// Only [`crate::store::TaskAgentResolution::Resolved`] outcomes are stored so that a transient
    /// [`crate::store::TaskAgentResolution::NotLinked`] (edges not yet written) is never pinned.
    task_agent_id_cache: DashMap<TaskId, AgentId>,
    /// Immutable after first successful `archive_ensure_prefix` in this process.
    archive_prefix_cache: DashMap<(String, String), u32>,
    /// Serializes `archive_next_local` per `(context_id, archive_prefix)` to cut MVCC/CREATE races.
    archive_local_serializers: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// One in-flight allocate per `(context_id, activity_anchor)`; pairs with unique DB index.
    archive_anchor_serializers: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl SurrealProvenanceStore {
    /// Access the underlying SurrealDB connection for direct queries (graph export, tool index).
    pub fn db(&self) -> &Surreal<Any> {
        &self.db
    }

    /// Run one SurrealQL statement with no binds; return statement `0` as JSON rows.
    pub(super) async fn query_sql_rows_mapped<E>(
        &self,
        sql: &str,
        map_err: impl Fn(surrealdb::Error) -> E + Copy,
    ) -> std::result::Result<Vec<Value>, E> {
        let response = self.db.query(sql).await.map_err(map_err)?;
        check_and_take_zero(response, map_err)
    }

    pub(super) async fn query_sql_rows(&self, sql: &str) -> Result<Vec<Value>> {
        self.query_sql_rows_mapped(sql, map_surreal_error).await
    }

    /// Message nodes scoped to a context with this `a2a_activity_anchor`, if any.
    pub(super) async fn existing_message_node_id_for_activity_anchor(
        &self,
        context_id: &ContextId,
        activity_anchor: &str,
    ) -> Result<Option<String>> {
        let ctx_node = crate::id_semantics::context_entity_id_string(context_id.as_str());
        let q = format!(
            "SELECT node_id FROM {TBL_NODE} WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node AND rel_type = $scoped_rel AND from_label = 'Message'\
             ) AND props.a2a_activity_anchor = $anchor LIMIT 1"
        );
        let response = self
            .db
            .query(&q)
            .bind(("ctx_node", ctx_node))
            .bind(("scoped_rel", context_scope::SCOPED_TO))
            .bind(("anchor", activity_anchor.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.first().and_then(|r| {
            r.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }))
    }

    /// Fails the write if another Message node already owns this activity anchor under the context
    /// scope with a different `node_id` than the event-derived message entity id.
    pub(super) async fn enforce_message_activity_anchor_invariant(
        &self,
        event: &crate::events::ProvEvent,
    ) -> Result<()> {
        let (ctx, msg_id) = match event.data() {
            crate::events::ProvEventData::MessageReceived { id, .. }
            | crate::events::ProvEventData::MessageSent { id, .. } => {
                let Some(ctx) = event.context_id_opt() else {
                    return Ok(());
                };
                (ctx, id)
            }
            _ => return Ok(()),
        };
        let anchor = event.id().as_str();
        let expected = ProvEntityId::derived::<MessageEntityId>(MessageEntityInput {
            context_id: ctx,
            message_id: msg_id,
        });
        let Some(existing) = self
            .existing_message_node_id_for_activity_anchor(ctx, anchor)
            .await?
        else {
            return Ok(());
        };
        if existing != expected.as_str() {
            return Err(ProvenanceError::MessageActivityAnchorConflict {
                activity_anchor: anchor.to_string(),
                context_id: ctx.as_str().to_string(),
                existing_node_id: existing,
                expected_entity_id: expected.as_str().to_string(),
            });
        }
        Ok(())
    }

    pub(super) async fn run_event_write_plan(
        &self,
        plan: impl crate::surreal_write_batch::ExecutableSurrealPlan,
    ) -> Result<()> {
        let (sql, binds) = plan.into_sql_and_binds();
        if sql.trim().is_empty() {
            return Ok(());
        }
        for attempt in 0..WRITE_CONFLICT_MAX_ATTEMPTS {
            let mut q = self.db.query(&sql);
            for bind in &binds {
                q = q.bind((bind.name.clone(), bind.value.clone()));
            }
            match q.await.and_then(surrealdb::IndexedResults::check) {
                Ok(_) => return Ok(()),
                Err(e) if is_transaction_conflict(&e) => {
                    if attempt + 1 >= WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: e.message().to_string(),
                        });
                    }
                    let delay = jittered_backoff(attempt);
                    tracing::debug!(
                        attempt = attempt + 1,
                        max_attempts = WRITE_CONFLICT_MAX_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        "provenance write hit MVCC conflict; retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }
        }
        unreachable!("retry loop returns on every iteration");
    }
}

#[cfg(test)]
impl SurrealProvenanceStore {
    pub(crate) fn task_agent_id_cache_len_for_test(&self) -> usize {
        self.task_agent_id_cache.len()
    }
}
