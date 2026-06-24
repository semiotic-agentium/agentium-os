// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Render provenance conversation items into wire transcript text.

use std::sync::Arc;

use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::ids::ContextId;
use baml_rt_tools::{
    archive_refs::RefTable,
    prompt_projection::{format_conversation_history_transcript, project_prompt_context},
    tools::ToolRegistry,
};

use crate::{
    error::Result, prepare_ref_table_for_projection, surreal_store::SurrealProvenanceStore,
};

/// One hydrated rendering context for a stable item set.
pub struct CompactionRenderContext {
    ref_table: Arc<RefTable>,
}

impl CompactionRenderContext {
    #[must_use]
    pub fn render_items(
        &self,
        items: &[ProvenanceConversationContextItem],
        tool_registry: &ToolRegistry,
    ) -> String {
        render_items_with_ref_table(items, tool_registry, self.ref_table.as_ref())
    }
}

/// Render items with a prepared ref table (sync; no store hydration).
#[must_use]
pub fn render_items_with_ref_table(
    items: &[ProvenanceConversationContextItem],
    tool_registry: &ToolRegistry,
    ref_table: &RefTable,
) -> String {
    let projection_items: Vec<_> = items
        .iter()
        .filter_map(|item| baml_rt_conversation::provenance_item_to_projection_item(item.clone()))
        .collect();
    if projection_items.is_empty() {
        return String::new();
    }
    let history = project_prompt_context(projection_items, tool_registry, ref_table, None);
    format_conversation_history_transcript(history.as_array().unwrap_or(&vec![]))
}

/// Hydrate ref table from store and render items for compaction or prompt measurement.
pub async fn render_items_for_context(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    items: &[ProvenanceConversationContextItem],
    tool_registry: &ToolRegistry,
) -> Result<String> {
    let projection_items: Vec<_> = items
        .iter()
        .filter_map(|item| baml_rt_conversation::provenance_item_to_projection_item(item.clone()))
        .collect();
    if projection_items.is_empty() {
        return Ok(String::new());
    }
    let ref_table =
        prepare_ref_table_for_projection(store, context_id, &projection_items, tool_registry)
            .await?;
    Ok(render_items_with_ref_table(
        items,
        tool_registry,
        ref_table.as_ref(),
    ))
}

/// Hydrate the ref table once for a stable item set and reuse it for full/prefix/tail renders.
pub async fn prepare_render_context(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    items: &[ProvenanceConversationContextItem],
    tool_registry: &ToolRegistry,
) -> Result<Option<CompactionRenderContext>> {
    let projection_items: Vec<_> = items
        .iter()
        .filter_map(|item| baml_rt_conversation::provenance_item_to_projection_item(item.clone()))
        .collect();
    if projection_items.is_empty() {
        return Ok(None);
    }
    let ref_table =
        prepare_ref_table_for_projection(store, context_id, &projection_items, tool_registry)
            .await?;
    Ok(Some(CompactionRenderContext { ref_table }))
}

/// Render wire JSON rows (post-`project_prompt_context`) to transcript bytes.
#[must_use]
pub fn render_wire_history_rows(rows: &[serde_json::Value]) -> String {
    format_conversation_history_transcript(rows)
}

/// Byte length of wire JSON rows (used for pre-model emergency threshold checks).
#[must_use]
pub fn wire_history_byte_len(rows: &[serde_json::Value]) -> u64 {
    render_wire_history_rows(rows).len() as u64
}
