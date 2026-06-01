// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Rebuild a session [`RefTable`] from durable provenance (graph-backed refs).

use std::sync::Arc;

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::ids::ContextId;
use baml_rt_tools::{
    archive_read::ShortRef,
    archive_refs::{HistoryEntry, RefTable},
    prompt_projection::{PromptProjectionContent, PromptProjectionItem},
};

use super::SurrealProvenanceStore;
use crate::{error::Result, store::ProvenanceContextReader, surreal_tables::TBL_ARCHIVE_BODY};

/// Load all archive bodies for a context into `table`.
async fn hydrate_archive_bodies(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    table: &RefTable,
) -> Result<()> {
    use serde_json::Value;

    use super::{
        archive_ref::entry_from_json,
        helpers::{check_and_take_zero, map_surreal_error},
    };

    let ctx = context_id.as_str();
    let q = format!(
        "SELECT archive_prefix, archive_local, entry FROM {TBL_ARCHIVE_BODY} \
         WHERE context_id = $ctx"
    );
    let response = store
        .db()
        .query(&q)
        .bind(("ctx", ctx.to_string()))
        .await
        .map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    for row in rows {
        let prefix = row
            .get("archive_prefix")
            .and_then(|x| x.as_u64())
            .unwrap_or(1);
        let local = row
            .get("archive_local")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let prefix = u32::try_from(prefix).unwrap_or(1);
        let local = u32::try_from(local).unwrap_or(0);
        if local == 0 {
            continue;
        }
        let archive_ref = ShortRef::new_prefixed(prefix, local);
        let Some(entry_json) = row.get("entry") else {
            continue;
        };
        if let Ok(entry) = entry_from_json(entry_json) {
            table.insert_virtual_archive_ref(archive_ref, entry);
        }
    }
    Ok(())
}

/// Rebuild [`RefTable`] from Surreal history registry + archive bodies + optional live text.
pub async fn hydrate_ref_table(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
) -> Result<Arc<RefTable>> {
    let table = Arc::new(RefTable::new());
    let rows = store.history_ref_list_for_context(context_id).await?;
    for (anchor, source, n) in rows {
        let entry = HistoryEntry::new(anchor.clone(), source);
        if let Some(text) = store
            .history_text_for_activity_anchor(context_id, anchor.as_str())
            .await?
        {
            table.insert_virtual_history(n, entry, text);
        } else {
            table.insert_virtual_history(n, entry, "");
        }
    }
    hydrate_archive_bodies(store, context_id, table.as_ref()).await?;
    Ok(table)
}

impl SurrealProvenanceStore {
    /// Best-effort prompt text for a history line from graph conversation rows.
    pub async fn history_text_for_activity_anchor(
        &self,
        context_id: &ContextId,
        activity_anchor: &str,
    ) -> Result<Option<String>> {
        let items = self.conversation_context(context_id, None).await?;
        for item in items {
            if item.activity_anchor.as_str() != activity_anchor {
                continue;
            }
            if let Some(text) = conversation_item_history_text(&item) {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }
}

fn conversation_item_history_text(item: &ProvenanceConversationContextItem) -> Option<String> {
    match &item.content {
        ConversationItemContent::Message { text, .. } => {
            let t = text.trim();
            if t.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        ConversationItemContent::ToolCall(tc) => Some(format!("{} {}", tc.tool_name, tc.args)),
        _ => None,
    }
}

/// DB-first ref table for prompt projection: sync `#N` registry, hydrate cache, overlay live text.
pub async fn prepare_ref_table_for_projection(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    projection_items: &[PromptProjectionItem],
    registry: &baml_rt_tools::tools::ToolRegistry,
) -> Result<Arc<RefTable>> {
    let mut history_entries = Vec::new();
    for item in projection_items {
        match &item.content {
            PromptProjectionContent::Message { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                history_entries.push((
                    item.activity_anchor.clone(),
                    "message".to_string(),
                    text.clone(),
                ));
            }
            PromptProjectionContent::ToolCall { tool_name, args } => {
                let mut desc =
                    registry.describe_invocation_with_hint(Some(tool_name.as_str()), args);
                if desc.trim().is_empty() {
                    desc = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
                }
                if desc.trim().is_empty() {
                    continue;
                }
                history_entries.push((item.activity_anchor.clone(), "tool_call".to_string(), desc));
            }
            _ => {}
        }
    }

    let sync_pairs: Vec<(String, String)> = history_entries
        .iter()
        .map(|(a, s, _)| (a.clone(), s.clone()))
        .collect();
    store
        .sync_history_refs_for_projection(context_id, &sync_pairs)
        .await?;

    let table = hydrate_ref_table(store, context_id).await?;
    for (anchor, source, content) in history_entries {
        let n = store
            .history_ref_ensure(context_id, anchor.as_str(), source.as_str())
            .await?;
        let entry = HistoryEntry::new(anchor, source);
        table.insert_virtual_history(n, entry, content);
    }
    Ok(table)
}
