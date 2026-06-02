// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Materialized context picker index — updated on message writes.

use serde_json::Value;

use super::{SurrealProvenanceStore, helpers::map_surreal_error};
use crate::{
    error::Result,
    events::{ProvEvent, ProvEventData},
    surreal_tables::TBL_CONTEXT_PICKER_INDEX,
};

impl SurrealProvenanceStore {
    pub(super) async fn update_context_picker_index(&self, event: &ProvEvent) -> Result<()> {
        let Some(context_id) = event.context_id_opt() else {
            return Ok(());
        };
        let (timestamp_ms, preview, role_user, has_host_ingress) = match event.data() {
            ProvEventData::MessageReceived { content, role, .. }
            | ProvEventData::MessageSent { content, role, .. } => {
                let preview = message_preview(content);
                let role_user = role.eq_ignore_ascii_case("user");
                (event.timestamp_ms(), preview, role_user, false)
            }
            ProvEventData::HostSourcePollRecorded { .. }
            | ProvEventData::HostDispatchAccepted { .. }
            | ProvEventData::HostDispatchRejected { .. } => (
                event.timestamp_ms(),
                "Host ingress event".to_string(),
                false,
                true,
            ),
            _ => return Ok(()),
        };

        let ctx = context_id.as_str().to_string();
        let existing = self.load_context_picker_row(&ctx).await?;
        let mut merged = merge_picker_row(
            existing.as_ref(),
            timestamp_ms,
            &preview,
            role_user,
            has_host_ingress,
        );
        merged.context_id = ctx;

        let sql = format!(
            "UPSERT {TBL_CONTEXT_PICKER_INDEX} SET \
               context_id = $context_id, \
               latest_timestamp_ms = $latest_timestamp_ms, \
               latest_preview = $latest_preview, \
               first_user_timestamp_ms = $first_user_timestamp_ms, \
               first_user_message = $first_user_message, \
               has_host_ingress = $has_host_ingress \
             WHERE context_id = $context_id"
        );
        self.db
            .query(&sql)
            .bind(("context_id", merged.context_id))
            .bind(("latest_timestamp_ms", merged.latest_timestamp_ms))
            .bind(("latest_preview", merged.latest_preview))
            .bind(("first_user_timestamp_ms", merged.first_user_timestamp_ms))
            .bind(("first_user_message", merged.first_user_message))
            .bind(("has_host_ingress", merged.has_host_ingress))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    async fn load_context_picker_row(&self, context_id: &str) -> Result<Option<ContextPickerRow>> {
        let sql = format!(
            "SELECT context_id, latest_timestamp_ms, latest_preview, first_user_message, has_host_ingress \
             FROM {TBL_CONTEXT_PICKER_INDEX} WHERE context_id = $context_id LIMIT 1"
        );
        let response = self
            .db
            .query(&sql)
            .bind(("context_id", context_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = super::helpers::check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.first().and_then(|row| {
            Some(ContextPickerRow {
                context_id: row.get("context_id")?.as_str()?.to_string(),
                latest_timestamp_ms: row
                    .get("latest_timestamp_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                latest_preview: row
                    .get("latest_preview")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                first_user_message: row
                    .get("first_user_message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                has_host_ingress: row
                    .get("has_host_ingress")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                first_user_timestamp_ms: row
                    .get("first_user_timestamp_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
            })
        }))
    }

    pub async fn page_context_picker_index(
        &self,
        offset: usize,
        limit: usize,
        event_only: bool,
        chat_only: bool,
    ) -> Result<Vec<ContextPickerIndexRow>> {
        let rows = self
            .page_context_picker_index_rows(offset, limit, event_only, chat_only)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn count_context_picker_index(
        &self,
        event_only: bool,
        chat_only: bool,
    ) -> Result<usize> {
        self.count_context_picker_index_rows(event_only, chat_only)
            .await
    }

    pub(super) async fn page_context_picker_index_rows(
        &self,
        offset: usize,
        limit: usize,
        event_only: bool,
        chat_only: bool,
    ) -> Result<Vec<ContextPickerRow>> {
        debug_assert!(
            !(event_only && chat_only),
            "event_only and chat_only are mutually exclusive"
        );
        let filter = context_picker_ingress_filter_sql(event_only, chat_only);
        let sql = format!(
            "SELECT context_id, latest_timestamp_ms, latest_preview, first_user_message, has_host_ingress, first_user_timestamp_ms \
             FROM {TBL_CONTEXT_PICKER_INDEX} {filter} \
             ORDER BY latest_timestamp_ms DESC \
             LIMIT $limit START $offset"
        );
        let response = self
            .db
            .query(&sql)
            .bind(("offset", offset))
            .bind(("limit", limit))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = super::helpers::check_and_take_zero(response, map_surreal_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(ContextPickerRow {
                    context_id: row.get("context_id")?.as_str()?.to_string(),
                    latest_timestamp_ms: row
                        .get("latest_timestamp_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    latest_preview: row
                        .get("latest_preview")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    first_user_message: row
                        .get("first_user_message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    has_host_ingress: row
                        .get("has_host_ingress")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    first_user_timestamp_ms: row
                        .get("first_user_timestamp_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX),
                })
            })
            .collect())
    }

    pub(super) async fn count_context_picker_index_rows(
        &self,
        event_only: bool,
        chat_only: bool,
    ) -> Result<usize> {
        debug_assert!(
            !(event_only && chat_only),
            "event_only and chat_only are mutually exclusive"
        );
        let filter = context_picker_ingress_filter_sql(event_only, chat_only);
        let sql =
            format!("SELECT count() AS total FROM {TBL_CONTEXT_PICKER_INDEX} {filter} GROUP ALL");
        let response = self.db.query(&sql).await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = super::helpers::check_and_take_zero(response, map_surreal_error)?;
        Ok(rows
            .first()
            .and_then(|r| r.get("total"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize)
    }
}

#[derive(Debug, Clone)]
pub struct ContextPickerIndexRow {
    pub context_id: String,
    pub latest_timestamp_ms: u64,
    pub latest_preview: String,
    pub first_user_message: String,
    pub has_host_ingress: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ContextPickerRow {
    pub context_id: String,
    pub latest_timestamp_ms: u64,
    pub latest_preview: String,
    pub first_user_message: String,
    pub has_host_ingress: bool,
    pub first_user_timestamp_ms: u64,
}

impl From<ContextPickerRow> for ContextPickerIndexRow {
    fn from(row: ContextPickerRow) -> Self {
        Self {
            context_id: row.context_id,
            latest_timestamp_ms: row.latest_timestamp_ms,
            latest_preview: row.latest_preview,
            first_user_message: row.first_user_message,
            has_host_ingress: row.has_host_ingress,
        }
    }
}

fn merge_picker_row(
    existing: Option<&ContextPickerRow>,
    timestamp_ms: u64,
    preview: &str,
    role_user: bool,
    has_host_ingress: bool,
) -> ContextPickerRow {
    let Some(existing) = existing else {
        return ContextPickerRow {
            context_id: String::new(),
            latest_timestamp_ms: timestamp_ms,
            latest_preview: preview.to_string(),
            first_user_message: if role_user {
                preview.to_string()
            } else {
                String::new()
            },
            has_host_ingress,
            first_user_timestamp_ms: if role_user && timestamp_ms > 0 {
                timestamp_ms
            } else {
                u64::MAX
            },
        };
    };
    let mut merged = existing.clone();
    if has_host_ingress {
        merged.has_host_ingress = true;
    }
    if timestamp_ms >= merged.latest_timestamp_ms {
        merged.latest_timestamp_ms = timestamp_ms;
        merged.latest_preview = preview.to_string();
    }
    if role_user
        && !preview.trim().is_empty()
        && timestamp_ms > 0
        && timestamp_ms <= merged.first_user_timestamp_ms
    {
        merged.first_user_timestamp_ms = timestamp_ms;
        merged.first_user_message = preview.to_string();
    }
    merged
}

fn context_picker_ingress_filter_sql(event_only: bool, chat_only: bool) -> &'static str {
    match (event_only, chat_only) {
        (true, _) => "WHERE has_host_ingress = true",
        (_, true) => "WHERE has_host_ingress = false",
        _ => "",
    }
}

fn message_preview(content: &[String]) -> String {
    let one_line = content
        .iter()
        .flat_map(|p| p.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ");
    if one_line.is_empty() {
        "Untitled conversation".to_string()
    } else if one_line.chars().count() > 80 {
        format!("{}...", one_line.chars().take(77).collect::<String>())
    } else {
        one_line
    }
}
