//! Load unified graph observations for operator surfaces.

use baml_rt_conversation::view::ProvenanceConversationContextItem;

use super::{
    scope::observation_scope_from_history,
    transcript_order::{sort_transcript_items, transcript_delta_rows},
    types::{
        EventOrder, LoadedObservation, ObservationScope, TaskObservationMetrics,
        TaskObservationScope, TemporalBound,
    },
};
use crate::{
    episode::token_summary_for_task, error::Result, surreal_store::SurrealProvenanceStore,
};

impl SurrealProvenanceStore {
    /// Load one observation for the given scope.
    pub async fn load_observation(&self, scope: ObservationScope) -> Result<LoadedObservation> {
        let ObservationScope {
            context_id,
            task,
            agent_package,
            temporal,
        } = scope.clone();

        let after_u64 = temporal.after_event_order().map(EventOrder::as_u64);
        let agent_pkg = agent_package.as_deref();
        let task_id = task.task_id();

        let mut transcript = self
            .conversation_context_filtered(&context_id, None, task_id, agent_pkg, after_u64, false)
            .await?;
        sort_transcript_items(&mut transcript);

        let max_event_order = EventOrder(
            transcript
                .iter()
                .map(|item| item.timestamp_ms)
                .max()
                .unwrap_or(0),
        );

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
            transcript,
            max_event_order,
            metrics,
        })
    }

    /// Delta rows after `after` from a full-scope load (single graph read).
    pub async fn load_observation_delta(
        &self,
        scope: ObservationScope,
        after: EventOrder,
        limit: usize,
    ) -> Result<(LoadedObservation, Vec<ProvenanceConversationContextItem>)> {
        let full_scope = ObservationScope {
            temporal: TemporalBound::All,
            ..scope
        };
        let loaded = self.load_observation(full_scope).await?;
        let delta = transcript_delta_rows(&loaded.transcript, after.as_u64(), limit);
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
