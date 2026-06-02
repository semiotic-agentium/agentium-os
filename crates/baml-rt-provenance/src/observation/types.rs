// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed operator observation scope and loaded graph slice.

use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use serde::{Deserialize, Serialize};

use crate::{store::ProvenanceOpsQueryResponse, surreal_store::TaskPlanningBatchRow};

/// Event ordering key (`a2a_event_order` on graph nodes). Named explicitly — not wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventOrder(pub u64);

impl EventOrder {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

/// Task filter for observation scope — explicit, not `Option<TaskId>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskObservationScope {
    ContextWide,
    Task(TaskId),
}

impl TaskObservationScope {
    #[must_use]
    pub fn task_id(&self) -> Option<&TaskId> {
        match self {
            Self::ContextWide => None,
            Self::Task(id) => Some(id),
        }
    }
}

/// Temporal bound on transcript rows (`a2a_event_order`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TemporalBound {
    #[default]
    All,
    After(EventOrder),
}

impl TemporalBound {
    #[must_use]
    pub fn after_event_order(self) -> Option<EventOrder> {
        match self {
            Self::All => None,
            Self::After(order) => Some(order),
        }
    }
}

/// Canonical operator observe scope — one struct for HTTP, runner services, and web.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationScope {
    pub context_id: ContextId,
    pub task: TaskObservationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_package: Option<String>,
    #[serde(default)]
    pub temporal: TemporalBound,
}

impl ObservationScope {
    #[must_use]
    pub fn context_wide(
        context_id: ContextId,
        agent_package: Option<String>,
        temporal: TemporalBound,
    ) -> Self {
        Self {
            context_id,
            task: TaskObservationScope::ContextWide,
            agent_package,
            temporal,
        }
    }

    #[must_use]
    pub fn for_task(
        context_id: ContextId,
        task_id: TaskId,
        agent_package: Option<String>,
        temporal: TemporalBound,
    ) -> Self {
        Self {
            context_id,
            task: TaskObservationScope::Task(task_id),
            agent_package,
            temporal,
        }
    }

    #[must_use]
    pub fn task_id(&self) -> Option<&TaskId> {
        self.task.task_id()
    }

    #[must_use]
    pub fn scope_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.context_id.as_str(),
            self.task_id().map(TaskId::as_str).unwrap_or(""),
            self.agent_package.as_deref().unwrap_or("")
        )
    }
}

/// Task-scoped metrics from graph aggregates (episode / TASK_CALL semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskObservationMetrics {
    pub llm_call_count: u32,
}

/// Loaded observation for one scope.
#[derive(Debug, Clone)]
pub struct LoadedObservation {
    pub scope: ObservationScope,
    pub transcript: Vec<ProvenanceConversationContextItem>,
    pub max_event_order: EventOrder,
    pub metrics: Option<TaskObservationMetrics>,
}

impl LoadedObservation {
    #[must_use]
    pub fn llm_call_count(&self) -> u32 {
        self.metrics.map(|m| m.llm_call_count).unwrap_or(0)
    }
}

/// Content-addressable observation version for SSE invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationVersion(pub String);

impl ObservationVersion {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Global vs context-scoped provenance ops query.
#[derive(Debug, Clone)]
pub enum OpsQueryMode {
    Global(crate::store::ProvenanceOpsQueryRequest),
    ContextScoped {
        scope: ObservationScope,
        request: crate::store::ProvenanceOpsQueryRequest,
    },
}

/// Operator bundle load request (store boundary — mirrors HTTP observe intent).
#[derive(Debug, Clone)]
pub struct ObservationBundleRequest {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub agent_package: Option<String>,
    pub agent_id: Option<AgentId>,
    pub include_planning: bool,
    pub include_drift: bool,
    pub include_llm_ops: bool,
    pub include_tool_ops: bool,
    pub ops_page_size: u32,
    pub planning_history_limit: u32,
}

/// Planning slice from index-backed batch read.
#[derive(Debug, Clone)]
pub struct LoadedPlanningSlice {
    pub all_task_ids: Vec<String>,
    pub tasks: Vec<TaskPlanningBatchRow>,
}

/// Loaded operator bundle (planning + ops projections).
#[derive(Debug, Clone)]
pub struct LoadedObservationBundle {
    pub context_id: ContextId,
    pub version: ObservationVersion,
    pub planning: Option<LoadedPlanningSlice>,
    pub llm_ops: Option<ProvenanceOpsQueryResponse>,
    pub tool_ops: Option<ProvenanceOpsQueryResponse>,
}
