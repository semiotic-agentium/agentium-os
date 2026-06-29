// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Rebuild a session [`RefTable`] from durable provenance (graph-backed refs).

use std::{collections::HashMap, sync::Arc};

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

/// Rebuild [`RefTable`] from Surreal history registry + archive bodies.
///
/// `known_text` maps `activity_anchor -> text` for anchors the caller already has
/// in hand (the prompt projection it just built). Only registry anchors **absent**
/// from `known_text` — history that fell outside the projection cap — force a
/// conversation re-read. In the common case (conversation within the cap) the read
/// is skipped entirely. Pass `&HashMap::new()` for a cold rebuild (e.g. restart),
/// which falls back to reading the full conversation once.
pub async fn hydrate_ref_table(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    known_text: &HashMap<String, String>,
) -> Result<Arc<RefTable>> {
    let table = Arc::new(RefTable::new());
    let rows = store.history_ref_list_for_context(context_id).await?;

    // The caller already fetched text for every in-projection anchor. Only read
    // the conversation when some registry anchor is *not* covered — i.e. history
    // beyond the projection cap. When everything is covered, skip the (O(N), and
    // growing) read altogether. When a read is needed it is a single indexed pass
    // (first-wins on duplicate anchors), never one-read-per-row (was O(N²)).
    let needs_read = rows
        .iter()
        .any(|(anchor, _, _)| !known_text.contains_key(anchor.as_str()));

    let fallback_text: HashMap<String, String> = if needs_read {
        let items = store.conversation_context(context_id, None).await?;
        let mut map: HashMap<String, String> = HashMap::with_capacity(items.len());
        for item in &items {
            if let Some(text) = conversation_item_history_text(item) {
                map.entry(item.activity_anchor.as_str().to_owned())
                    .or_insert(text);
            }
        }
        map
    } else {
        HashMap::new()
    };

    for (anchor, source, n) in rows {
        let text = known_text
            .get(anchor.as_str())
            .or_else(|| fallback_text.get(anchor.as_str()))
            .cloned()
            .unwrap_or_default();
        table.insert_virtual_history(n, HistoryEntry::new(anchor, source), text);
    }

    hydrate_archive_bodies(store, context_id, table.as_ref()).await?;
    Ok(table)
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
            PromptProjectionContent::Planning(plan) => {
                if plan.summary.trim().is_empty() {
                    continue;
                }
                let mut body = format!("[planning:{}] {}", plan.kind.as_wire_str(), plan.summary);
                if let Some(ref detail) = plan.detail
                    && !detail.trim().is_empty()
                {
                    body.push_str(&format!(" — {detail}"));
                }
                history_entries.push((item.activity_anchor.clone(), "plan".to_string(), body));
            }
            PromptProjectionContent::CompactionSummary { summary, .. } => {
                if summary.trim().is_empty() {
                    continue;
                }
                history_entries.push((
                    item.activity_anchor.clone(),
                    "compaction".to_string(),
                    summary.clone(),
                ));
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

    // Text the caller already has for every in-projection anchor; lets
    // `hydrate_ref_table` skip the conversation re-read unless older (capped-out)
    // history needs hydrating. The overlay loop below re-asserts these anyway, so
    // last-wins on duplicate anchors is fine here.
    let known_text: HashMap<String, String> = history_entries
        .iter()
        .map(|(anchor, _source, content)| (anchor.as_str().to_owned(), content.clone()))
        .collect();

    let table = hydrate_ref_table(store, context_id, &known_text).await?;
    for (anchor, source, content) in history_entries {
        let n = store
            .history_ref_ensure(context_id, anchor.as_str(), source.as_str())
            .await?;
        let entry = HistoryEntry::new(anchor, source);
        table.insert_virtual_history(n, entry, content);
    }
    Ok(table)
}
