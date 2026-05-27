//! Load unified graph observations for operator surfaces.

use baml_rt_conversation::view::ProvenanceConversationContextItem;

use super::{
    scope::observation_scope_from_history,
    transcript_order::transcript_delta_rows,
    types::{
        EventOrder, LoadedObservation, ObservationScope, TaskObservationMetrics,
        TaskObservationScope,
    },
};
use crate::{
    episode::token_summary_for_task,
    error::Result,
    read::{TranscriptReader, TranscriptSliceSpec},
    surreal_store::SurrealProvenanceStore,
};

impl SurrealProvenanceStore {
    fn transcript_spec_from_scope(
        &self,
        scope: &ObservationScope,
        after_event_order: u64,
        limit: usize,
        include_extensions: bool,
    ) -> TranscriptSliceSpec {
        TranscriptSliceSpec {
            context_id: scope.context_id.clone(),
            task_id: scope.task_id().cloned(),
            agent_package: scope.agent_package.clone(),
            after_event_order,
            limit,
            include_extensions,
        }
    }

    /// Load one observation for the given scope (bounded slice; no full-context scan).
    pub async fn load_observation(&self, scope: ObservationScope) -> Result<LoadedObservation> {
        let after_u64 = scope
            .temporal
            .after_event_order()
            .map(EventOrder::as_u64)
            .unwrap_or(0);
        let limit = usize::MAX / 4;
        let spec = self.transcript_spec_from_scope(&scope, after_u64, limit, true);
        let slice = TranscriptReader::slice(self, spec).await?;

        let ObservationScope { task, .. } = scope.clone();
        let metrics = match task {
            TaskObservationScope::Task(ref tid) => Some(TaskObservationMetrics {
                llm_call_count: token_summary_for_task(self, tid.as_str())
                    .await?
                    .llm_call_count,
            }),
            TaskObservationScope::ContextWide => None,
        };

        Ok(LoadedObservation {
            scope,
            transcript: slice.items,
            max_event_order: EventOrder(slice.max_event_order),
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
        let spec = self.transcript_spec_from_scope(&scope, after.as_u64(), limit, false);
        let slice = TranscriptReader::slice(self, spec).await?;
        let delta = transcript_delta_rows(&slice.items, after.as_u64(), limit);

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
            transcript: slice.items.clone(),
            max_event_order: EventOrder(slice.max_event_order),
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
