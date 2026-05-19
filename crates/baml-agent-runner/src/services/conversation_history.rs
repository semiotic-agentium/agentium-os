//! Conversation history service backed by provenance query API.

use std::sync::Arc;

use baml_rt_provenance::{
    ProvenanceQueryApi as _, context_metrics_queries, resolve_resume_ui_hints,
};

use super::metrics::{value_as_string, value_as_u64};

pub(crate) struct ConversationHistoryServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ConversationHistoryServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    async fn enrich_prompt_metrics(
        &self,
        page: &mut baml_rt_api::ConversationHistoryPageDto,
        after_exclusive: Option<u64>,
    ) -> Result<(), baml_rt_api::ConversationHistoryError> {
        let ctx = page.context_id.as_str();
        let tid = page.task_id.as_deref();

        let tail = context_metrics_queries::session_prompt_context_tail(&self.store, ctx, tid)
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        page.prompt_context_bytes_session_current = tail
            .as_ref()
            .map(|r| value_as_u64(r.get("prompt_context_bytes_current")));
        page.prompt_message_chars_session_current = tail
            .as_ref()
            .map(|r| value_as_u64(r.get("prompt_message_chars_current")));

        let max_eo = page.max_event_order;
        let op_rows = context_metrics_queries::llm_prompt_operations_for_context(
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

        page.version = baml_rt_api::page_version(
            &page.items,
            &page.llm_prompt_operations,
            page.prompt_context_bytes_session_current,
            page.prompt_message_chars_session_current,
            page.awaiting_input,
            page.input_required_prompt.as_deref(),
        );
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
        page.version = baml_rt_api::page_version(
            &page.items,
            &page.llm_prompt_operations,
            page.prompt_context_bytes_session_current,
            page.prompt_message_chars_session_current,
            page.awaiting_input,
            page.input_required_prompt.as_deref(),
        );
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
        let rows = self
            .store
            .query_conversation_context(&request.context_id, None, request.task_id.as_ref())
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        let mut page = baml_rt_api::paginate_items(rows, request)?;
        if matches!(
            request.profile,
            baml_rt_api::ConversationHistoryProfile::Compact
        ) {
            page.items = page
                .items
                .into_iter()
                .map(|item| baml_rt_api::profile_filter(item, request.profile))
                .collect();
        }
        self.enrich_prompt_metrics(&mut page, None).await?;
        self.enrich_resume_ui_hints(
            &mut page,
            request.context_id.as_str(),
            request.task_id.as_ref(),
        )
        .await?;
        Ok(page)
    }

    async fn delta_after_event_order(
        &self,
        request: &baml_rt_api::ConversationHistoryDeltaRequest,
    ) -> std::result::Result<
        baml_rt_api::ConversationHistoryPageDto,
        baml_rt_api::ConversationHistoryError,
    > {
        let rows = self
            .store
            .query_conversation_context_after(
                &request.context_id,
                request.after_event_order,
                Some(request.limit),
                request.task_id.as_ref(),
            )
            .await
            .map_err(|e| baml_rt_api::ConversationHistoryError::Other(Box::new(e)))?;

        let mut items = rows
            .into_iter()
            .map(baml_rt_api::ConversationHistoryItemDto::from)
            .collect::<Vec<_>>();
        if matches!(
            request.profile,
            baml_rt_api::ConversationHistoryProfile::Compact
        ) {
            items = items
                .into_iter()
                .map(|item| baml_rt_api::profile_filter(item, request.profile))
                .collect();
        }
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
        Ok(page)
    }
}
