//! Context picker index service backed by provenance ops reads.

use std::{collections::HashMap, sync::Arc};

use baml_rt_provenance::{
    ProvenanceOpsFilters, ProvenanceOpsQuery as _, ProvenanceOpsQueryRequest,
    ProvenanceOpsResource, ProvenanceOutcomeSegment, ProvenanceResponseProfile,
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

#[async_trait::async_trait]
impl baml_rt_api::ContextIndexService for ContextIndexServiceImpl {
    async fn page(
        &self,
        request: &baml_rt_api::ContextIndexRequest,
    ) -> std::result::Result<baml_rt_api::ContextPickerPageDto, baml_rt_api::ContextIndexError>
    {
        let mut cursor: Option<String> = None;
        let mut scanned_rows = 0usize;
        let max_rows = 5000usize;
        let mut grouped: HashMap<String, ContextAggregate> = HashMap::new();

        loop {
            // The `agent_package` filter is pushed into the SurrealQL
            // query as an edge traversal
            // (Message ↔ MessageProcessing -> AgentRuntimeInstance ->
            // AgentBoot -> AgentArchive), so the picker receives only
            // matching rows. No per-row property filter on
            // `props.a2a_agent_id` is needed (or supported — that
            // property is not written on Message entities).
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
                    filters: ProvenanceOpsFilters {
                        agent_package: request.agent_package.clone(),
                        ..Default::default()
                    },
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

                let entry =
                    grouped
                        .entry(context_id.to_string())
                        .or_insert_with(|| ContextAggregate {
                            context_id: context_id.to_string(),
                            latest_timestamp_ms: timestamp_ms,
                            latest_preview: preview.clone(),
                            first_user_timestamp_ms: u64::MAX,
                            first_user_message: String::new(),
                        });

                if timestamp_ms >= entry.latest_timestamp_ms {
                    entry.latest_timestamp_ms = timestamp_ms;
                    entry.latest_preview = preview.clone();
                }
                if role == "ROLE_USER"
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
                )
                .0,
            )
        } else {
            None
        };

        Ok(baml_rt_api::ContextPickerPageDto { items, next_cursor })
    }
}
