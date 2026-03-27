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
//! **Payload reads:** Use deterministic `payload_id` (`format!("payload:{anchor}:{kind}")`),
//! not `WHERE activity_anchor_id = $anchor AND payload_kind = $kind`.
//!
//! **Never** filter nodes by `props.a2a_context_id`, `props.a2a_task_id`, or other
//! relational-crutch properties in query WHERE clauses. These properties exist as
//! informational attributes for display/audit only; relationships are expressed as edges.

mod a2a_store;
mod builder;
mod context_reader;
mod helpers;
mod ops_query;
mod payload;
mod planning_query;
mod schema;
mod writer;

use std::sync::Arc;

pub use builder::{SurrealBackend, SurrealStoreBuilder};
use serde_json::Value;
use surrealdb::{Surreal, engine::local::Db};

use self::helpers::{map_surreal_error, query_take_zero};
use crate::{error::Result, mermaid_cache::MermaidCache, normalizer::ProvNormalizer};

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

    /// Run one SurrealQL statement with no binds; return statement `0` as JSON rows.
    pub(super) async fn query_sql_rows_mapped<E>(
        &self,
        sql: &str,
        map_err: impl Fn(surrealdb::Error) -> E + Copy,
    ) -> std::result::Result<Vec<Value>, E> {
        let mut response = self.db.query(sql).await.map_err(map_err)?;
        query_take_zero(&mut response, map_err)
    }

    pub(super) async fn query_sql_rows(&self, sql: &str) -> Result<Vec<Value>> {
        self.query_sql_rows_mapped(sql, map_surreal_error).await
    }

    pub(super) async fn run_event_write_plan(
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
}
