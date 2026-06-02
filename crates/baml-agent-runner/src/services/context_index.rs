// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Context picker index service backed by materialized `context_picker_index` table.

use std::{collections::HashMap, sync::Arc};

use baml_rt_api::ContextPickerIngressFilter;
use baml_rt_provenance::{
    ContextPickerIndexRow, ProvenanceOpsFilters, ProvenanceOpsQuery as _,
    ProvenanceOpsQueryRequest, ProvenanceOpsResource, ProvenanceOutcomeSegment,
    ProvenanceResponseProfile,
};
use serde_json::Value;

pub(crate) struct ContextIndexServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ContextIndexServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone)]
struct ContextAggregate {
    context_id: String,
    latest_timestamp_ms: u64,
    latest_preview: String,
    first_user_timestamp_ms: u64,
    first_user_message: String,
    has_host_ingress: bool,
}

fn is_host_ingress_activity_anchor(
    activity_id: &str,
    row: &baml_rt_provenance::ProvenanceOpsRow,
) -> bool {
    if activity_id.starts_with("ingress-poll-user:")
        || activity_id.starts_with("ingress-unit-user:")
    {
        return true;
    }
    row.get("a2a_user_speaker_kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("ingress"))
}

fn normalize_preview(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        "Untitled conversation".to_string()
    } else if one_line.chars().count() > 80 {
        format!("{}...", one_line.chars().take(77).collect::<String>())
    } else {
        one_line
    }
}

fn as_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

fn picker_rows_to_items(rows: &[ContextPickerIndexRow]) -> Vec<baml_rt_api::ContextPickerItemDto> {
    rows.iter()
        .map(|ctx| baml_rt_api::ContextPickerItemDto {
            context_id: ctx.context_id.clone(),
            latest_timestamp_ms: ctx.latest_timestamp_ms,
            preview: if !ctx.first_user_message.is_empty() {
                ctx.first_user_message.clone()
            } else {
                ctx.latest_preview.clone()
            },
        })
        .collect()
}

fn ingress_filter_flags(filter: ContextPickerIngressFilter) -> (bool, bool) {
    (filter.event_only(), filter.chat_only())
}

#[async_trait::async_trait]
impl baml_rt_api::ContextIndexService for ContextIndexServiceImpl {
    async fn page(
        &self,
        request: &baml_rt_api::ContextIndexRequest,
    ) -> std::result::Result<baml_rt_api::ContextPickerPageDto, baml_rt_api::ContextIndexError>
    {
        let (event_only, chat_only) = ingress_filter_flags(request.ingress_filter);
        let force_message_scan = request.agent_package.is_some();

        if request.agent_package.is_some() {
            tracing::debug!(
                agent_package = request.agent_package.as_deref(),
                "context picker: message ops scan (agent package filter)"
            );
        }

        if !force_message_scan {
            let indexed_count = self
                .store
                .count_context_picker_index(event_only, chat_only)
                .await
                .map_err(|e| {
                    baml_rt_api::ContextIndexError::Other(Box::new(std::io::Error::other(e)))
                })?;

            if indexed_count > 0 {
                let rows = self
                    .store
                    .page_context_picker_index(request.offset, request.limit, event_only, chat_only)
                    .await
                    .map_err(|e| {
                        baml_rt_api::ContextIndexError::Other(Box::new(std::io::Error::other(e)))
                    })?;
                let items = picker_rows_to_items(&rows);
                let next_offset = request.offset.saturating_add(items.len());
                let next_cursor = if next_offset < indexed_count {
                    Some(
                        baml_rt_api::ContextIndexCursorToken::encode_v1(
                            next_offset,
                            request.agent_package.as_deref(),
                            request.ingress_filter,
                        )
                        .0,
                    )
                } else {
                    None
                };
                return Ok(baml_rt_api::ContextPickerPageDto { items, next_cursor });
            }
        }

        self.page_via_message_ops_scan(request).await
    }
}

impl ContextIndexServiceImpl {
    async fn page_via_message_ops_scan(
        &self,
        request: &baml_rt_api::ContextIndexRequest,
    ) -> std::result::Result<baml_rt_api::ContextPickerPageDto, baml_rt_api::ContextIndexError>
    {
        let (event_only, chat_only) = ingress_filter_flags(request.ingress_filter);
        let mut scan_filters = ProvenanceOpsFilters::default();
        if let Some(pkg) = request.agent_package.as_deref() {
            scan_filters.agent_package = Some(pkg.to_string());
        }

        let mut cursor: Option<String> = None;
        let mut scanned_rows = 0usize;
        let max_rows = 5000usize;
        let mut grouped: HashMap<String, ContextAggregate> = HashMap::new();

        loop {
            let response = self
                .store
                .query_ops(ProvenanceOpsQueryRequest {
                    resource: ProvenanceOpsResource::Messages,
                    sort_by: Some("timestamp_ms".to_string()),
                    sort_dir: Some("desc".to_string()),
                    page_size: Some(500),
                    cursor: cursor.clone(),
                    outcome: Some(ProvenanceOutcomeSegment::Both),
                    response_profile: Some(ProvenanceResponseProfile::UiFull),
                    filters: scan_filters.clone(),
                    ..Default::default()
                })
                .await
                .map_err(|e| {
                    baml_rt_api::ContextIndexError::Other(Box::new(std::io::Error::other(e)))
                })?;

            for row in response.rows {
                scanned_rows = scanned_rows.saturating_add(1);
                let context_id = row
                    .get("context_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty());
                let Some(context_id) = context_id else {
                    continue;
                };

                let timestamp_ms = as_u64(row.get("timestamp_ms"));
                let message_text = row
                    .get("message_text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let preview = normalize_preview(message_text);
                let role = row
                    .get("a2a_role")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let activity_id = row
                    .get("activity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let ingress_row = is_host_ingress_activity_anchor(activity_id, &row);

                let entry =
                    grouped
                        .entry(context_id.to_string())
                        .or_insert_with(|| ContextAggregate {
                            context_id: context_id.to_string(),
                            latest_timestamp_ms: timestamp_ms,
                            latest_preview: preview.clone(),
                            first_user_timestamp_ms: u64::MAX,
                            first_user_message: String::new(),
                            has_host_ingress: false,
                        });

                if ingress_row {
                    entry.has_host_ingress = true;
                }

                if timestamp_ms >= entry.latest_timestamp_ms {
                    entry.latest_timestamp_ms = timestamp_ms;
                    entry.latest_preview = preview.clone();
                }
                if role == "ROLE_USER"
                    && !ingress_row
                    && !message_text.trim().is_empty()
                    && timestamp_ms > 0
                    && timestamp_ms <= entry.first_user_timestamp_ms
                {
                    entry.first_user_timestamp_ms = timestamp_ms;
                    entry.first_user_message = preview.clone();
                }
            }

            if scanned_rows >= max_rows {
                break;
            }
            let Some(next_cursor) = response.next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }

        let mut contexts = grouped.into_values().collect::<Vec<_>>();
        if event_only {
            contexts.retain(|ctx| ctx.has_host_ingress);
        } else if chat_only {
            contexts.retain(|ctx| !ctx.has_host_ingress);
        }
        contexts.sort_by_key(|ctx| std::cmp::Reverse(ctx.latest_timestamp_ms));

        if request.offset > contexts.len() {
            return Ok(baml_rt_api::ContextPickerPageDto {
                items: Vec::new(),
                next_cursor: None,
            });
        }

        let end = request
            .offset
            .saturating_add(request.limit)
            .min(contexts.len());
        let items = contexts[request.offset..end]
            .iter()
            .map(|ctx| baml_rt_api::ContextPickerItemDto {
                context_id: ctx.context_id.clone(),
                latest_timestamp_ms: ctx.latest_timestamp_ms,
                preview: if !ctx.first_user_message.is_empty() {
                    ctx.first_user_message.clone()
                } else {
                    ctx.latest_preview.clone()
                },
            })
            .collect::<Vec<_>>();
        let next_cursor = if end < contexts.len() {
            Some(
                baml_rt_api::ContextIndexCursorToken::encode_v1(
                    end,
                    request.agent_package.as_deref(),
                    request.ingress_filter,
                )
                .0,
            )
        } else {
            None
        };

        Ok(baml_rt_api::ContextPickerPageDto { items, next_cursor })
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_provenance::ProvenanceOpsRow;
    use serde_json::json;

    use super::is_host_ingress_activity_anchor;

    fn ingress_row(value: serde_json::Value) -> ProvenanceOpsRow {
        ProvenanceOpsRow::from_map(value.as_object().cloned().unwrap_or_default())
    }

    #[test]
    fn host_ingress_activity_anchors() {
        assert!(is_host_ingress_activity_anchor(
            "ingress-poll-user:ctx-1:msg-1",
            &ingress_row(json!({})),
        ));
        assert!(is_host_ingress_activity_anchor(
            "ingress-unit-user:ctx-1:unit-1",
            &ingress_row(json!({})),
        ));
        assert!(is_host_ingress_activity_anchor(
            "derived-host-ingress-anchor",
            &ingress_row(json!({"a2a_user_speaker_kind": "ingress"})),
        ));
        assert!(!is_host_ingress_activity_anchor(
            "a2a:user-turn:1",
            &ingress_row(json!({})),
        ));
    }
}
