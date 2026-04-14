//! Conversation history service backed by provenance query API.

use std::sync::Arc;

use baml_rt_provenance::ProvenanceQueryApi as _;

pub(crate) struct ConversationHistoryServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ConversationHistoryServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
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
        Ok(page)
    }
}
