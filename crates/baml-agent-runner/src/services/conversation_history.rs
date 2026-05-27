// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation history service backed by provenance observation loader.

use std::sync::Arc;

use baml_rt_provenance::{
    EventOrder, LoadedObservation, ObservationLoader as _, ObservationScope, PageVersionEnvelope,
    PromptOpsVersionRow, ResumeVersionHints, TemporalBound, observation_scope_from_history,
    observation_version_page, resolve_resume_ui_hints,
};

use super::metrics::{value_as_string, value_as_u64};

pub(crate) struct ConversationHistoryServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ConversationHistoryServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    fn scope_from_request(
        request: &baml_rt_api::ConversationHistoryRequest,
        temporal: TemporalBound,
    ) -> ObservationScope {
        ObservationScope {
            context_id: request.context_id.clone(),
            task: match request.task_id.clone() {
                Some(id) => baml_rt_provenance::TaskObservationScope::Task(id),
                None => baml_rt_provenance::TaskObservationScope::ContextWide,
            },
            agent_package: request.agent_package.clone(),
            temporal,
        }
    }

    fn finalize_version(
        page: &mut baml_rt_api::ConversationHistoryPageDto,
        loaded: &LoadedObservation,
    ) {
        page.llm_call_count = loaded.llm_call_count();
        let prompt_ops: Vec<PromptOpsVersionRow<'_>> = page
            .llm_prompt_operations
            .iter()
            .map(|op| PromptOpsVersionRow {
                activity_anchor: &op.activity_anchor,
                event_order: op.event_order,
                prompt_context_bytes_current: op.prompt_context_bytes_current,
                prompt_message_chars_current: op.prompt_message_chars_current,
            })
            .collect();
        page.version = observation_version_page(
            &loaded.transcript,
            loaded.metrics.as_ref(),
            PageVersionEnvelope {
                prompt_ops: &prompt_ops,
                prompt_context_bytes_session_current: page.prompt_context_bytes_session_current,
                prompt_message_chars_session_current: page.prompt_message_chars_session_current,
                resume: ResumeVersionHints {
                    awaiting_input: page.awaiting_input,
                    input_required_prompt: page.input_required_prompt.as_deref(),
                },
            },
        )
        .as_str()
        .to_string();
    }

    async fn enrich_prompt_metrics(
        &self,
        page: &mut baml_rt_api::ConversationHistoryPageDto,
        after_exclusive: Option<u64>,
    ) -> Result<(), baml_rt_api::ConversationHistoryError> {
        let ctx = page.context_id.as_str();
        let tid = page.task_id.as_deref();

        let tail = baml_rt_provenance::context_metrics_queries::session_prompt_context_tail(
            &self.store,
            ctx,
            tid,
        )
        .await
        .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        page.prompt_context_bytes_session_current = tail
            .as_ref()
            .map(|r| value_as_u64(r.get("prompt_context_bytes_current")));
        page.prompt_message_chars_session_current = tail
            .as_ref()
            .map(|r| value_as_u64(r.get("prompt_message_chars_current")));

        let max_eo = page.max_event_order;
        let op_rows =
            baml_rt_provenance::context_metrics_queries::llm_prompt_operations_for_context(
                &self.store,
                ctx,
                tid,
                max_eo,
                after_exclusive,
            )
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        page.llm_prompt_operations = op_rows
            .into_iter()
            .filter_map(|row| {
                let anchor = value_as_string(row.get("activity_anchor"));
                if anchor.is_empty() {
                    return None;
                }
                Some(baml_rt_api::LlmPromptOperationDto {
                    activity_anchor: anchor,
                    event_order: value_as_u64(row.get("event_order")),
                    prompt_context_bytes_current: value_as_u64(
                        row.get("prompt_context_bytes_current"),
                    ),
                    prompt_message_chars_current: value_as_u64(
                        row.get("prompt_message_chars_current"),
                    ),
                })
            })
            .collect();

        if let Some(ref row) = tail {
            let te = value_as_u64(row.get("event_order"));
            page.max_event_order = page.max_event_order.max(te);
        }
        for op in &page.llm_prompt_operations {
            page.max_event_order = page.max_event_order.max(op.event_order);
        }

        Ok(())
    }

    async fn enrich_resume_ui_hints(
        &self,
        page: &mut baml_rt_api::ConversationHistoryPageDto,
        request_context_id: &str,
        request_task_id: Option<&baml_rt_core::ids::TaskId>,
    ) -> Result<(), baml_rt_api::ConversationHistoryError> {
        let hints = resolve_resume_ui_hints(
            &self.store,
            request_context_id,
            request_task_id.map(|t| t.as_str()),
        )
        .await
        .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        if page.task_id.is_none() {
            page.task_id = hints.effective_task_id.clone();
        }
        page.awaiting_input = hints.awaiting_input;
        page.input_required_prompt = hints.input_required_prompt;
        Ok(())
    }
}

#[async_trait::async_trait]
impl baml_rt_api::ConversationHistoryService for ConversationHistoryServiceImpl {
    async fn page(
        &self,
        request: &baml_rt_api::ConversationHistoryRequest,
    ) -> std::result::Result<
        baml_rt_api::ConversationHistoryPageDto,
        baml_rt_api::ConversationHistoryError,
    > {
        let loaded = self
            .store
            .load(Self::scope_from_request(request, TemporalBound::All))
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        let mut page = baml_rt_api::paginate_items(
            loaded.transcript.clone(),
            request,
            loaded.llm_call_count(),
        )?;
        page.items = baml_rt_api::apply_conversation_history_profile(page.items, request.profile);

        self.enrich_prompt_metrics(&mut page, None).await?;
        self.enrich_resume_ui_hints(
            &mut page,
            request.context_id.as_str(),
            request.task_id.as_ref(),
        )
        .await?;
        Self::finalize_version(&mut page, &loaded);
        Ok(page)
    }

    async fn delta_after_event_order(
        &self,
        request: &baml_rt_api::ConversationHistoryDeltaRequest,
    ) -> std::result::Result<
        baml_rt_api::ConversationHistoryPageDto,
        baml_rt_api::ConversationHistoryError,
    > {
        let scope = observation_scope_from_history(
            request.context_id.clone(),
            request.task_id.clone(),
            request.agent_package.clone(),
            None,
        );
        let (loaded, delta_rows) = self
            .store
            .load_delta(scope, EventOrder(request.after_event_order), request.limit)
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        let mut items = delta_rows
            .into_iter()
            .map(baml_rt_api::ConversationHistoryItemDto::from)
            .collect::<Vec<_>>();
        items = baml_rt_api::apply_conversation_history_profile(items, request.profile);

        let max_event_order = items
            .iter()
            .map(|item| item.timestamp_ms)
            .max()
            .unwrap_or(0);

        let mut page = baml_rt_api::ConversationHistoryPageDto {
            context_id: request.context_id.as_str().to_string(),
            task_id: request.task_id.as_ref().map(|id| id.as_str().to_string()),
            version: String::new(),
            max_event_order,
            items,
            next_cursor: None,
            prompt_context_bytes_session_current: None,
            prompt_message_chars_session_current: None,
            llm_prompt_operations: Vec::new(),
            awaiting_input: false,
            input_required_prompt: None,
            llm_call_count: loaded.llm_call_count(),
        };

        let after_exclusive = if max_event_order > request.after_event_order {
            Some(request.after_event_order)
        } else {
            None
        };

        self.enrich_prompt_metrics(&mut page, after_exclusive)
            .await?;
        self.enrich_resume_ui_hints(
            &mut page,
            request.context_id.as_str(),
            request.task_id.as_ref(),
        )
        .await?;
        Self::finalize_version(&mut page, &loaded);
        Ok(page)
    }
}
