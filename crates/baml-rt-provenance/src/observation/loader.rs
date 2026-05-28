//! Load unified graph observations for operator surfaces.

use baml_rt_conversation::view::ProvenanceConversationContextItem;

use super::{
    scope::observation_scope_from_history,
    transcript_order::transcript_delta_rows,
    types::{
        EventOrder, LoadedObservation, ObservationScope, TaskObservationMetrics,
        TaskObservationScope, TemporalBound,
    },
};
use crate::{
    episode::token_summary_for_task,
    error::Result,
    read::{TranscriptEngine, TranscriptPageRequest, TranscriptProjectionProfile},
    surreal_store::SurrealProvenanceStore,
};

impl SurrealProvenanceStore {
    /// Load one observation for the given scope (bounded slice; no full-context scan).
    pub async fn load_observation(&self, scope: ObservationScope) -> Result<LoadedObservation> {
        let limit = usize::MAX / 4;
        let page = TranscriptEngine::page(
            self,
            TranscriptPageRequest {
                scope: scope.clone(),
                limit,
                profile: TranscriptProjectionProfile::OperatorTimeline,
            },
        )
        .await?;

        let metrics = match scope.task {
            TaskObservationScope::Task(ref tid) => Some(TaskObservationMetrics {
                llm_call_count: token_summary_for_task(self, tid.as_str())
                    .await?
                    .llm_call_count,
            }),
            TaskObservationScope::ContextWide => None,
        };

        Ok(LoadedObservation {
            scope,
            transcript: page.items,
            max_event_order: EventOrder(page.max_event_order),
            metrics,
        })
    }

    /// Delta rows after `after` via a single bounded index slice (no full reload).
    pub async fn load_observation_delta(
        &self,
        scope: ObservationScope,
        after: EventOrder,
        limit: usize,
    ) -> Result<(LoadedObservation, Vec<ProvenanceConversationContextItem>)> {
        let mut scoped = scope.clone();
        scoped.temporal = TemporalBound::After(after);
        let page = TranscriptEngine::page(
            self,
            TranscriptPageRequest {
                scope: scoped,
                limit,
                profile: TranscriptProjectionProfile::LiveStructuralDelta,
            },
        )
        .await?;
        let delta = transcript_delta_rows(&page.items, after.as_u64(), limit);

        let metrics = match scope.task {
            TaskObservationScope::Task(ref tid) => Some(TaskObservationMetrics {
                llm_call_count: token_summary_for_task(self, tid.as_str())
                    .await?
                    .llm_call_count,
            }),
            TaskObservationScope::ContextWide => None,
        };

        let loaded = LoadedObservation {
            scope,
            transcript: page.items.clone(),
            max_event_order: EventOrder(page.max_event_order),
            metrics,
        };
        Ok((loaded, delta))
    }

    /// Task metrics without loading transcript (planning / lightweight ops paths).
    pub async fn load_task_metrics(
        &self,
        scope: &ObservationScope,
    ) -> Result<Option<TaskObservationMetrics>> {
        let Some(task_id) = scope.task_id() else {
            return Ok(None);
        };
        Ok(Some(TaskObservationMetrics {
            llm_call_count: token_summary_for_task(self, task_id.as_str())
                .await?
                .llm_call_count,
        }))
    }

    /// Build scope from conversation-history query parameters.
    #[must_use]
    pub fn observation_scope_from_history(
        context_id: baml_rt_core::ids::ContextId,
        task_id: Option<baml_rt_core::ids::TaskId>,
        agent_package: Option<String>,
        after_event_order: Option<u64>,
    ) -> ObservationScope {
        observation_scope_from_history(context_id, task_id, agent_package, after_event_order)
    }
}
