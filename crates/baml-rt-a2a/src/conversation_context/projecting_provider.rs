// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Projects provenance conversation rows into wire JSON for BAML tags.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::{Result, context};
use baml_rt_provenance::DEFAULT_LLM_CONTEXT_ITEM_CAP;
use baml_rt_quickjs::baml_execution::ConversationContextProvider;
use baml_rt_tools::{
    prompt_projection::{PromptProjectionItem, project_prompt_context},
    tools::ToolRegistry,
};
use serde_json::Value;

use super::compaction_gate::{
    prompt_exceeds_emergency_threshold, run_pre_model_emergency_compaction,
};
use crate::a2a_store::{ConversationContextSource, ProvenanceWriterConversationSource};

type BoxedArchiveReader = Box<dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>>;

/// Projects [`ProvenanceConversationContextItem`] rows into **wire** conversation JSON.
pub struct ProjectingConversationContextProvider {
    source: Arc<dyn ConversationContextSource>,
    tool_registry: Arc<ToolRegistry>,
    provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
    archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
    compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
}

impl ProjectingConversationContextProvider {
    pub fn new(
        source: Arc<dyn ConversationContextSource>,
        tool_registry: Arc<ToolRegistry>,
        provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
        archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
        compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
    ) -> Self {
        Self {
            source,
            tool_registry,
            provenance_store,
            archive_ref_tables,
            compaction_subscriber,
        }
    }

    pub fn from_provenance_writer(
        writer: Arc<dyn baml_rt_provenance::ProvenanceWriter>,
        tool_registry: Arc<ToolRegistry>,
        provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
        archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
        compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
    ) -> Self {
        let source: Arc<dyn ConversationContextSource> =
            Arc::new(ProvenanceWriterConversationSource::new(writer));
        Self::new(
            source,
            tool_registry,
            provenance_store,
            archive_ref_tables,
            compaction_subscriber,
        )
    }

    async fn project_conversation_to_json(
        &self,
        scope: &context::RuntimeScope,
        item_limit: Option<usize>,
    ) -> Result<Option<Value>> {
        let context_id = scope.context_id();
        let task_id = scope.task_id_opt();
        let items = if let Some(store) = self.provenance_store.as_ref() {
            store
                .conversation_context_for_agent_prompt(context_id, item_limit, task_id)
                .await
                .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                    source: Box::new(e),
                })?
        } else {
            self.source
                .conversation_context_with_task(context_id, item_limit, task_id)
                .await?
        };
        tracing::debug!(
            context_id = %context_id,
            task_id = ?task_id.map(|t| t.as_str()),
            item_count = items.len(),
            item_limit = ?item_limit,
            "project_conversation_to_json: context source returned items"
        );
        if items.is_empty() {
            return Ok(None);
        }

        let projection_items = items
            .into_iter()
            .filter_map(to_projection_item)
            .collect::<Vec<_>>();
        if projection_items.is_empty() {
            return Ok(None);
        }

        let context_id_str = context_id.as_str().to_string();

        let ref_table_arc = if let Some(store) = self.provenance_store.as_ref() {
            let prepared = baml_rt_provenance::prepare_ref_table_for_projection(
                store,
                context_id,
                &projection_items,
                self.tool_registry.as_ref(),
            )
            .await
            .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
            if let Some(tables) = self.archive_ref_tables.as_ref() {
                tables.insert(context_id_str.clone(), Arc::clone(&prepared));
            }
            prepared
        } else if let Some(tables) = self.archive_ref_tables.as_deref() {
            baml_rt_tools::archive_refs::get_or_create_ref_table(tables, &context_id_str)
        } else {
            Arc::new(baml_rt_tools::archive_refs::RefTable::new())
        };

        let reader: Option<BoxedArchiveReader> = self.archive_ref_tables.clone().map(|t| {
            let ctx = context_id_str.clone();
            let boxed: BoxedArchiveReader =
                Box::new(move |archive_ref_str, grep_str, offset, limit| {
                    let ref_table = baml_rt_tools::archive_refs::get_ref_table(&t, &ctx)?;
                    baml_rt_tools::archive_read::format_session_read_from_vtable(
                        &ref_table,
                        archive_ref_str,
                        grep_str,
                        offset,
                        limit,
                    )
                });
            boxed
        });

        Ok(Some(project_prompt_context(
            projection_items,
            self.tool_registry.as_ref(),
            &ref_table_arc,
            reader.as_deref(),
        )))
    }
}

#[async_trait]
impl ConversationContextProvider for ProjectingConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        let json = self
            .project_conversation_to_json(scope, Some(DEFAULT_LLM_CONTEXT_ITEM_CAP))
            .await?;
        let needs_emergency = json
            .as_ref()
            .and_then(|v| v.as_array())
            .is_some_and(|rows| {
                self.compaction_subscriber
                    .as_ref()
                    .is_some_and(|sub| prompt_exceeds_emergency_threshold(rows, sub.as_ref()))
            });
        if needs_emergency {
            if let Some(subscriber) = self.compaction_subscriber.as_ref() {
                let rows = json
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[]);
                run_pre_model_emergency_compaction(subscriber, scope, rows, false).await;
            }
            return self
                .project_conversation_to_json(scope, Some(DEFAULT_LLM_CONTEXT_ITEM_CAP))
                .await;
        }
        Ok(json)
    }

    async fn conversation_history_json_for_intra_dedup(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        self.project_conversation_to_json(scope, None).await
    }
}

/// Convert a provenance conversation item to a projection item.
pub fn to_projection_item(item: ProvenanceConversationContextItem) -> Option<PromptProjectionItem> {
    baml_rt_conversation::provenance_item_to_projection_item(item)
}
