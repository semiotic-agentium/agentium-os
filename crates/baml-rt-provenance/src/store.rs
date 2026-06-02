// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Provenance store traits: write events and read task/conversation context.
//!
//! ## Read intents: agent context vs API
//!
//! Two query paths are distinguished at the type level so the type system enforces the
//! intended behavior:
//!
//! - **[ProvenanceReadIntent::AgentContext]** (via [ProvenanceContextReader]): Used by the agent
//!   runtime for task and conversation context when building prompts. **No-stale-read invariant**
//!   applies: a read must reflect all prior completed writes. Implementations must enforce this
//!   (e.g. serialized worker so reads see prior writes).
//! - **[ProvenanceReadIntent::Api]** (via [ProvenanceQueryApi]): Exposed to APIs for display,
//!   analytics, or ad-hoc queries. **No guarantee** of no-stale-read; implementations may use
//!   read replicas, caches, or relaxed ordering. Other provenance queries do not require
//!   consistency.
//!
//! The **typed enum** [ProvenanceReadIntent] documents these two behaviors; the **two traits**
//! enforce at the type level: agent code holds [ProvenanceContextReader] (or [ProvenanceWriter]),
//! API code holds [ProvenanceQueryApi]. The same store can implement both.
//!
//! ## No-stale-read invariant (ProvenanceContextReader only)
//!
//! - **Property:** ∀ write W completed before read R via [ProvenanceContextReader]: R reflects W.
//! - **Enforcement:** Implementations must ensure any read that starts after a completed write
//!   sees that write (SurrealDB MVCC + awaiting [ProvenanceWriter::add_event]). Callers must await
//!   writes before reader methods when they need those events visible.
//!
//! ## Graph-first design constraint
//!
//! All **storage** provenance read paths must derive relationships from graph edge traversals.
//! String-based ID prefix matching (`starts_with`, `strip_prefix`), positional FIFO
//! matching, and tool-name suffix matching are prohibited on those read paths. If a
//! read-path projection is impossible given the stored graph, the graph construction
//! (write path) is incorrect — fix the write path, do not add a read-time heuristic.
//! **Episode / API view-model** code may apply display-only pairing (e.g. matching a synthetic
//! tool name to enrich JSON) as long as it does not substitute for graph-backed retrieval.
//!
//! ID conventions (e.g. `"session-step:{anchor}"`) may be used at **write time** to
//! construct deterministic node IDs — this is the normalizer's own convention, not a
//! read-path concern. These conventions must be expressed through typed semantic ID
//! constructors (see [`crate::id_semantics`]), not bare `format!()` strings.
//!
//! ## `Option` discipline
//!
//! `Option` on provenance types must represent **genuine optionality** — "this value
//! may be legitimately absent." It must not represent construction order ("not built
//! yet" — use typestate), variant-specific data ("only for SendDone" — use enum
//! variants), or fallible side-channel retrieval ("DashMap might not have it" — fix
//! the insertion or assert).
//!
//! When a provenance event field is `None` at a point where it should be populated,
//! the write path must either: (1) reject the event with an error, or (2) log at
//! `error!` level and degrade gracefully with a synthetic fallback. Silently mapping
//! `None` to "skip this edge/attribute" is prohibited — it produces invisible graph
//! corruption that manifests as missing data on the read path, far from the source.

use async_trait::async_trait;
use baml_rt_conversation::view::{ProvenanceContextMessage, ProvenanceConversationContextItem};
use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{ActivityAnchorId, AgentId, ContextId, TaskId},
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{ProvenanceError, Result},
    events::ProvEvent,
    task_agent_binding::{TaskAgentBinding, TaskAgentBindingSource},
};

/// Outcome of resolving the agent that owns a task via graph traversal.
///
/// Distinguishes "no agent linked in the graph" from "query timed out" so
/// callers on the write path can log degradation explicitly rather than
/// silently skipping agent-scoped normalization.
#[derive(Debug, Clone)]
pub enum TaskAgentResolution {
    Resolved(AgentId),
    NotLinked,
    TimedOut,
}

impl TaskAgentResolution {
    /// Write path only — agent-scoped normalization may proceed without a resolved head.
    pub fn for_normalization(self) -> Option<AgentId> {
        match self {
            Self::Resolved(id) => Some(id),
            Self::NotLinked | Self::TimedOut => None,
        }
    }

    /// Episode assembly gate — unbound tasks are not episodes.
    pub fn require_for_episode(self, task_id: &TaskId) -> Result<AgentId> {
        match self {
            Self::Resolved(id) => Ok(id),
            Self::NotLinked => Err(ProvenanceError::EpisodeUnbound {
                task_id: task_id.clone(),
            }),
            Self::TimedOut => Err(ProvenanceError::EpisodeAgentResolutionTimedOut {
                task_id: task_id.clone(),
            }),
        }
    }
}

#[async_trait]
pub trait ProvenanceWriter: ProvenanceContextReader + Send + Sync {
    async fn add_event(&self, event: ProvEvent) -> Result<()>;

    async fn add_events(&self, events: Vec<ProvEvent>) -> Result<()> {
        for event in events {
            self.add_event(event).await?;
        }
        Ok(())
    }

    async fn add_event_with_logging(&self, event: ProvEvent, context: &str) {
        if let Err(e) = self.add_event(event).await {
            tracing::warn!(error = ?e, context = context, "Failed to record provenance event");
        }
    }

    /// Idempotent: ensures `TaskExists` + `TaskExecutionStarted` → `WAS_LAST_EXECUTED_BY`.
    async fn bind_task_executing_agent(&self, binding: TaskAgentBinding) -> Result<()> {
        tracing::debug!(
            task_id = binding.task_id.as_str(),
            agent_id = binding.executing_agent_id.as_str(),
            source = ?binding.source,
            "bind_task_executing_agent"
        );
        for event in binding.into_events() {
            self.add_event(event).await?;
        }
        Ok(())
    }

    /// Convenience wrapper for A2A chat bootstrap paths.
    async fn bind_task_executing_agent_for(
        &self,
        context_id: ContextId,
        task_id: TaskId,
        executing_agent_id: AgentId,
        source: TaskAgentBindingSource,
    ) -> Result<()> {
        self.bind_task_executing_agent(TaskAgentBinding::new(
            context_id,
            task_id,
            executing_agent_id,
            source,
        )?)
        .await
    }
}

/// Intent for a provenance read: enforces which guarantee the caller gets.
///
/// - **AgentContext:** No-stale-read required. Use via [ProvenanceContextReader]; implementations
///   must ensure reads reflect all prior completed writes (e.g. serialized with writes).
/// - **Api:** No consistency guarantee. Use via [ProvenanceQueryApi]; for APIs, display, analytics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvenanceReadIntent {
    /// Agent/task/conversation context: read must reflect all prior completed writes.
    AgentContext,
    /// API or analytics: no guarantee of no-stale-read.
    Api,
}

/// Reader for task and conversation context used by agents to build prompts.
///
/// **No-stale-read invariant:** A read of [context_messages] or [conversation_context] must
/// reflect all prior writes that completed before the read. This trait corresponds to
/// [ProvenanceReadIntent::AgentContext]. Use [ProvenanceQueryApi] for API-exposed reads that do
/// not require this guarantee.
///
/// **Graph-first contract:** Implementations must reconstruct conversation items from graph
/// structure (edges, node properties, stored `event_order`) — not from node ID string
/// conventions or positional matching. Node property reads (e.g. `a2a_event_order`,
/// `a2a_role`, `a2a_content`) are the canonical data source.
#[async_trait]
pub trait ProvenanceContextReader: Send + Sync {
    /// Messages for the given context (user + assistant). Used for conversation history.
    /// Must reflect all prior [ProvenanceWriter::add_event] calls that completed before this call.
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>>;

    /// Full conversation context (messages + tool calls) for the given context. Used for
    /// BAML conversation context. Must reflect all prior [ProvenanceWriter::add_event] calls
    /// that completed before this call. Returns **all** context-scoped rows (not filtered by task).
    ///
    /// For the same slice as [ProvenanceQueryApi::query_conversation_context] with a task filter,
    /// use [Self::conversation_context_with_task] when `task_id` is known (e.g. citation drift
    /// scoring for a task-scoped LLM completion).
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;

    /// Conversation rows for prompt-adjacent logic that must align with a **task transcript**
    /// (same filtering as [ProvenanceQueryApi::query_conversation_context] when `task_id` is `Some`).
    ///
    /// Implementations must preserve the task filter; widening to full-context
    /// history is not an acceptable fallback on this surface.
    async fn conversation_context_with_task(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}

/// Query API for provenance context: **does not** guarantee no-stale-read.
///
/// Use this trait for API-exposed reads (display, analytics, ad-hoc queries). Implementations
/// may use read replicas, caches, or relaxed ordering. Corresponds to [ProvenanceReadIntent::Api].
/// For agent/task context that requires no-stale-read, use [ProvenanceContextReader] instead.
#[async_trait]
pub trait ProvenanceQueryApi: Send + Sync {
    /// Messages for the given context. No guarantee that the result reflects the latest writes.
    async fn query_context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>>;

    /// Full conversation context for the given context. No guarantee of consistency with writes.
    ///
    /// When `task_id` is `Some`, only rows linked to that task are returned (for per-task
    /// transcript / episode assembly). When `None`, returns the same rows as
    /// [ProvenanceContextReader::conversation_context] (full context history).
    /// Agent-side code with only [ProvenanceContextReader] should use
    /// [ProvenanceContextReader::conversation_context_with_task] for the same filter under
    /// no-stale-read.
    async fn query_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;

    /// Incremental conversation rows strictly after `after_event_order` in ascending order.
    ///
    /// Default implementation falls back to filtering `query_conversation_context`.
    async fn query_conversation_context_after(
        &self,
        context_id: &ContextId,
        after_event_order: u64,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let rows = self
            .query_conversation_context(context_id, None, task_id, agent_package)
            .await?;
        let mut filtered = rows
            .into_iter()
            .filter(|row| row.timestamp_ms > after_event_order)
            .collect::<Vec<_>>();
        filtered.sort_by_key(|row| row.timestamp_ms);
        if let Some(n) = limit
            && filtered.len() > n
        {
            filtered.truncate(n);
        }
        Ok(filtered)
    }
}

/// Planning snapshot for explainability. `intent_id` is the **task-scoped planning alias** from
/// the execution-session wire (often agent-chosen or LLM-suggested text), **not** a standalone
/// global provenance key; pair with `task_id` (and derived intent entity id in the graph).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanningIntentRecord {
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub activity_anchor_id: ActivityAnchorId,
    pub intent_id: String,
    pub description: String,
    /// Monotonic event counter parsed from the activity anchor at write time.
    #[serde(default)]
    pub event_order: u64,
    /// Relation kind from the previous revision to this record, if any.
    pub supersession_from_previous: Option<PlanningSupersessionKind>,
    /// Relation kind from this record to a newer revision, if any.
    pub superseded_by_next: Option<PlanningSupersessionKind>,
}

/// Step row in a planning snapshot. `step_id` is the **plan-local alias** (may be LLM-authored);
/// it is not globally unique — canonical step entities in the graph use ids derived from
/// `(task_id, plan_id, step_id)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanningPlanStepRecord {
    pub step_id: String,
    pub description: String,
    pub order: u32,
    pub depends_on: Vec<String>,
    pub status: String,
}

/// Planning snapshot for a committed plan. `intent_id` and `plan_id` are **task-scoped aliases**
/// from the wire, not global provenance identifiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanningPlanRecord {
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub activity_anchor_id: ActivityAnchorId,
    pub intent_id: String,
    pub plan_id: String,
    pub steps: Vec<PlanningPlanStepRecord>,
    /// Monotonic event counter parsed from the activity anchor at write time.
    #[serde(default)]
    pub event_order: u64,
    /// Relation kind from the previous revision to this record, if any.
    pub supersession_from_previous: Option<PlanningSupersessionKind>,
    /// Relation kind from this record to a newer revision, if any.
    pub superseded_by_next: Option<PlanningSupersessionKind>,
}

/// Query API for planning state (intent/plan) and revision history views.
///
/// These are read-only explainability surfaces for UI/debug tools.
#[async_trait]
pub trait ProvenancePlanningQuery: Send + Sync {
    async fn query_current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>>;
    async fn query_current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>>;
    async fn query_intent_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningIntentRecord>>;
    async fn query_plan_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningPlanRecord>>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceOpsResource {
    LlmCalls,
    ToolCalls,
    Messages,
    Aggregates,
    LifecycleEvents,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceOutcomeSegment {
    FailedOnly,
    SuccessfulOnly,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceResponseProfile {
    UiFull,
    ToolCompact,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    /// Filter rows by the owning agent's *package* identifier (the value
    /// written to `AgentArchive.props.a2a_agent_type`). When the
    /// `agent_package_instance` registry is populated, ops queries resolve
    /// package → instance node ids once and filter via `for_agent_instances`
    /// instead of the nested archive/bootstrap subquery in
    /// `for_agent_package`. Without registry rows, the same two-hop edge
    /// chain is followed via graph traversal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baml_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_timestamp_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_timestamp_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsQueryRequest {
    pub resource: ProvenanceOpsResource,
    #[serde(default)]
    pub filters: ProvenanceOpsFilters,
    #[serde(default)]
    pub group_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProvenanceOutcomeSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ProvenanceResponseProfile>,
    #[serde(default = "default_true")]
    pub budget_mode: bool,
    /// When true, row fetch uses SQL LIMIT/OFFSET even when `group_by` is set (hotspots still computed on fetched rows).
    #[serde(default)]
    pub paginate_rows_in_sql: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ProvenanceOpsQueryRequest {
    fn default() -> Self {
        Self {
            resource: ProvenanceOpsResource::LlmCalls,
            filters: ProvenanceOpsFilters::default(),
            group_by: Vec::new(),
            sort_by: None,
            sort_dir: None,
            page_size: Some(50),
            cursor: None,
            top_k: Some(10),
            outcome: Some(ProvenanceOutcomeSegment::Both),
            response_profile: Some(ProvenanceResponseProfile::UiFull),
            budget_mode: true,
            paginate_rows_in_sql: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsQueryResponse {
    pub resource: ProvenanceOpsResource,
    pub rows: Vec<crate::ops_types::ProvenanceOpsRow>,
    pub summary: crate::ops_types::ProvenanceOpsSummary,
    pub hotspot_groups: Vec<crate::ops_types::ProvenanceOpsHotspotGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub applied_caps: crate::ops_types::ProvenanceOpsAppliedCaps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRef(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRef(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRef(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ProvenanceArchivePayload {
    LlmCall {
        payload_ref: PayloadRef,
        activity_ref: ActivityRef,
        prompt_json: String,
    },
    LlmResult {
        payload_ref: PayloadRef,
        activity_ref: ActivityRef,
        result_json: String,
    },
    ToolCall {
        payload_ref: PayloadRef,
        activity_ref: ActivityRef,
        tool_name: Option<String>,
        phase: Option<String>,
        args_json: String,
    },
    ToolResult {
        payload_ref: PayloadRef,
        activity_ref: ActivityRef,
        result_json: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceArchiveRecord {
    pub archive_ref: ArchiveRef,
    pub payloads: Vec<ProvenanceArchivePayload>,
}

/// Operational analytics queries over the provenance store.
///
/// **Graph-first contract:** Query results should derive relationships from node properties
/// and edge labels — not from node ID string parsing. Ordering uses the persisted
/// `event_order` property rather than activity-anchor string manipulation.
#[async_trait]
pub trait ProvenanceOpsQuery: Send + Sync {
    async fn query_ops(
        &self,
        request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse>;

    /// Resolve an opaque archive reference into its persisted payload.
    ///
    /// Supported refs are implementation-defined. The canonical contract is:
    /// - `prov:v1:payload:<payload_id>`
    /// - `prov:v1:activity:<activity_id>`
    async fn resolve_archive_ref(
        &self,
        _archive_ref: &str,
    ) -> Result<Option<ProvenanceArchiveRecord>> {
        Ok(None)
    }
}
