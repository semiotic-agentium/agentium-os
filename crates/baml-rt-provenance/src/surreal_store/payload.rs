// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Payload rows, archive refs, extraction from [`crate::events::ProvEvent`], and payload queries.

use std::collections::{HashMap, HashSet};

use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, SessionStepOp,
};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{
        check_and_take_zero, has_meaningful_result, map_surreal_error, normalize_payload_text_query,
    },
};
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEventData,
    payload_id::payload_row_id,
    payload_record::{PayloadRecord, StorageKind},
    payload_storage,
    store::{ActivityRef, PayloadRef, ProvenanceArchivePayload},
    surreal_tables::{
        FTS_PAYLOAD_ACTIVITY_WHERE, PAYLOAD_ROW_SELECT, TBL_PAYLOAD, TBL_PAYLOAD_BLOB,
    },
};

pub(super) fn payload_id_for(anchor: &str, payload_kind: &str) -> String {
    payload_row_id(anchor, payload_kind)
}

pub(super) fn archive_ref_for_payload(payload_id: &str) -> String {
    format!("prov:v1:payload:{payload_id}")
}

pub(super) fn archive_ref_for_activity(activity_id: &str) -> String {
    format!("prov:v1:activity:{activity_id}")
}
// ---------------------------------------------------------------------------
// Payload extraction from events
// ---------------------------------------------------------------------------

pub(super) fn merge_result_error_metadata(result: Option<Value>, error: Option<Value>) -> Value {
    match (result, error) {
        (Some(result), Some(error)) => serde_json::json!({ "result": result, "error": error }),
        (Some(result), None) => result,
        (None, Some(error)) => serde_json::json!({ "error": error }),
        (None, None) => Value::Null,
    }
}

pub(super) fn payload_records_from_event(
    event: &crate::events::ProvEvent,
) -> Result<Vec<PayloadRecord>> {
    let activity_anchor_id = event.id().as_str().to_string();
    match event.data() {
        ProvEventData::LlmCallStarted { prompt, .. } => {
            let payload_json =
                serde_json::to_string(prompt).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_call prompt: {e}"),
                })?;
            let search_text = payload_storage::search_text_snippet(&payload_json);
            Ok(vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_call"),
                activity_anchor_id,
                activity_id: None,
                payload_kind: "llm_call".to_string(),
                payload_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text,
            }])
        }
        ProvEventData::LlmCallCompleted {
            prompt, metadata, ..
        } => {
            let llm_call_json =
                serde_json::to_string(prompt).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_call: {e}"),
                })?;
            let llm_call_st = payload_storage::search_text_snippet(&llm_call_json);
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_call"),
                activity_anchor_id: activity_anchor_id.clone(),
                activity_id: None,
                payload_kind: "llm_call".to_string(),
                payload_json: llm_call_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: llm_call_st,
            }];
            let payload = merge_result_error_metadata(
                metadata.get("result").cloned(),
                metadata.get("error").cloned(),
            );
            let lr_json =
                serde_json::to_string(&payload).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize llm_result: {e}"),
                })?;
            let lr_st = payload_storage::search_text_snippet(&lr_json);
            out.push(PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "llm_result"),
                activity_anchor_id,
                activity_id: None,
                payload_kind: "llm_result".to_string(),
                payload_json: lr_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: lr_st,
            });
            Ok(out)
        }
        ProvEventData::ToolCallStarted {
            tool_name,
            args,
            metadata,
            ..
        }
        | ProvEventData::ToolCallCompleted {
            tool_name,
            args,
            metadata,
            ..
        } => {
            let phase = metadata.get("phase").cloned().unwrap_or(Value::Null);
            let tool_call = serde_json::json!({
                "name": tool_name,
                "args": args,
                "phase": phase
            });
            let tc_json =
                serde_json::to_string(&tool_call).map_err(|e| ProvenanceError::InvalidEvent {
                    activity_anchor: activity_anchor_id.clone(),
                    reason: format!("serialize tool_call: {e}"),
                })?;
            let tc_st = payload_storage::search_text_snippet(&tc_json);
            let mut out = vec![PayloadRecord {
                payload_id: payload_id_for(&activity_anchor_id, "tool_call"),
                activity_anchor_id: activity_anchor_id.clone(),
                activity_id: None,
                payload_kind: "tool_call".to_string(),
                payload_json: tc_json,
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: tc_st,
            }];
            if matches!(event.data(), ProvEventData::ToolCallCompleted { .. }) {
                let payload = merge_result_error_metadata(
                    metadata.get("result").cloned(),
                    metadata.get("error").cloned(),
                );
                let tr_json =
                    serde_json::to_string(&payload).map_err(|e| ProvenanceError::InvalidEvent {
                        activity_anchor: activity_anchor_id.clone(),
                        reason: format!("serialize tool_result: {e}"),
                    })?;
                let tr_st = payload_storage::search_text_snippet(&tr_json);
                out.push(PayloadRecord {
                    payload_id: payload_id_for(&activity_anchor_id, "tool_result"),
                    activity_anchor_id,
                    activity_id: None,
                    payload_kind: "tool_result".to_string(),
                    payload_json: tr_json,
                    content_hash: None,
                    storage_kind: StorageKind::Inline,
                    file_key: None,
                    search_text: tr_st,
                });
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

pub(super) fn archive_payload_from_record(
    payload: PayloadRecord,
) -> Result<ProvenanceArchivePayload> {
    let payload_ref = PayloadRef(archive_ref_for_payload(&payload.payload_id));
    let activity_id = payload
        .activity_id
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: payload.activity_anchor_id.clone(),
            reason: format!(
                "payload {} missing activity_id for kind {}",
                payload.payload_id, payload.payload_kind
            ),
        })?;
    let activity_ref = ActivityRef(archive_ref_for_activity(&activity_id));
    let payload_json = payload.payload_json;
    let parsed: Value =
        serde_json::from_str(&payload_json).unwrap_or_else(|_| Value::String(payload_json.clone()));
    match payload.payload_kind.as_str() {
        "llm_call" => Ok(ProvenanceArchivePayload::LlmCall {
            payload_ref,
            activity_ref,
            prompt_json: payload_json,
        }),
        "llm_result" => Ok(ProvenanceArchivePayload::LlmResult {
            payload_ref,
            activity_ref,
            result_json: payload_json,
        }),
        "tool_call" => {
            let tool_name = parsed
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let phase = parsed
                .get("phase")
                .and_then(Value::as_str)
                .map(str::to_string);
            let args = parsed.get("args").cloned().unwrap_or(Value::Null);
            let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "null".to_string());
            Ok(ProvenanceArchivePayload::ToolCall {
                payload_ref,
                activity_ref,
                tool_name,
                phase,
                args_json,
            })
        }
        "tool_result" => Ok(ProvenanceArchivePayload::ToolResult {
            payload_ref,
            activity_ref,
            result_json: payload_json,
        }),
        other => Err(ProvenanceError::InvalidEvent {
            activity_anchor: payload.activity_anchor_id.clone(),
            reason: format!("unsupported payload_kind for archive retrieval: {other}"),
        }),
    }
}

pub(super) enum ParsedArchiveRef<'a> {
    PayloadId(&'a str),
    ActivityId(&'a str),
}

pub(super) fn parse_archive_ref(archive_ref: &str) -> Option<ParsedArchiveRef<'_>> {
    if let Some(payload_id) = archive_ref.strip_prefix("prov:v1:payload:") {
        if payload_id.is_empty() {
            return None;
        }
        return Some(ParsedArchiveRef::PayloadId(payload_id));
    }
    if let Some(activity_id) = archive_ref.strip_prefix("prov:v1:activity:") {
        if activity_id.is_empty() {
            return None;
        }
        return Some(ParsedArchiveRef::ActivityId(activity_id));
    }
    None
}

pub(super) fn decode_payload_row(v: Value) -> Result<PayloadRecord> {
    serde_json::from_value(v).map_err(|e| ProvenanceError::CorruptPayloadRow {
        reason: e.to_string(),
    })
}

impl SurrealProvenanceStore {
    /// Batch-fetch blob bodies for offload-backed payload rows (one round trip).
    async fn read_payload_blob_bodies(
        &self,
        content_hashes: &[String],
    ) -> Result<HashMap<String, String>> {
        if content_hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let in_list = content_hashes
            .iter()
            .map(|h| format!("'{h}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT content_hash, body FROM {TBL_PAYLOAD_BLOB} WHERE content_hash IN [{in_list}]"
        );
        let rows: Vec<Value> = self.query_sql_rows(&query).await?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(hash) = row.get("content_hash").and_then(Value::as_str) else {
                continue;
            };
            let Some(body) = row.get("body").and_then(Value::as_str) else {
                continue;
            };
            out.insert(hash.to_string(), body.to_string());
        }
        Ok(out)
    }

    pub(crate) async fn hydrate_payload_record(
        &self,
        mut p: PayloadRecord,
    ) -> Result<PayloadRecord> {
        if let Some(ref h) = p.content_hash
            && !h.is_empty()
            && p.payload_json.is_empty()
        {
            let bodies = self
                .read_payload_blob_bodies(std::slice::from_ref(h))
                .await?;
            if let Some(body) = bodies.get(h) {
                p.payload_json = body.clone();
            }
        }
        Ok(p)
    }

    pub(crate) async fn hydrate_payload_records(
        &self,
        records: Vec<PayloadRecord>,
    ) -> Result<Vec<PayloadRecord>> {
        if records.is_empty() {
            return Ok(records);
        }
        let hashes: Vec<String> = records
            .iter()
            .filter_map(|p| {
                if p.content_hash.as_ref().is_some_and(|h| !h.is_empty())
                    && p.payload_json.is_empty()
                {
                    p.content_hash.clone()
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let bodies = self.read_payload_blob_bodies(&hashes).await?;
        Ok(records
            .into_iter()
            .map(|mut p| {
                if let Some(ref h) = p.content_hash
                    && p.payload_json.is_empty()
                    && let Some(body) = bodies.get(h)
                {
                    p.payload_json = body.clone();
                }
                p
            })
            .collect())
    }

    /// Fill [`SessionStepContent::send_done_replay_payload`] using deterministic payload_id
    /// computation from the `informed_by_tool_activity_anchor` property already on
    /// the SessionStep node (written by the normalizer).
    pub(crate) async fn hydrate_session_step_send_done_payloads(
        &self,
        items: &mut [ProvenanceConversationContextItem],
    ) -> Result<()> {
        let targets: Vec<(usize, String)> = items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let ConversationItemContent::SessionStep(ss) = &item.content else {
                    return None;
                };
                let SessionStepOp::SendDone { informed_by, .. } = &ss.op else {
                    return None;
                };
                if informed_by.is_empty() {
                    return None;
                }
                Some((idx, payload_id_for(informed_by.as_str(), "tool_result")))
            })
            .collect();

        if targets.is_empty() {
            return Ok(());
        }

        let in_list = targets
            .iter()
            .map(|(_, pid)| format!("'{pid}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id IN [{in_list}]"
        );
        let payload_rows: Vec<Value> = self.query_sql_rows(&query).await?;
        let decoded: Vec<PayloadRecord> = payload_rows
            .into_iter()
            .map(decode_payload_row)
            .collect::<Result<Vec<_>>>()?;
        let hydrated = self.hydrate_payload_records(decoded).await?;
        let mut payload_map: HashMap<String, PayloadRecord> = HashMap::new();
        for rec in hydrated {
            payload_map.insert(rec.payload_id.clone(), rec);
        }

        for (idx, payload_id) in &targets {
            let Some(payload_rec) = payload_map.get(payload_id) else {
                continue;
            };
            let ConversationItemContent::SessionStep(ss) = &mut items[*idx].content else {
                continue;
            };
            let parsed: Value =
                serde_json::from_str(&payload_rec.payload_json).unwrap_or(Value::Null);
            let val = parsed
                .get("result")
                .cloned()
                .unwrap_or_else(|| parsed.clone());
            if has_meaningful_result(&val) {
                ss.send_done_replay_payload = Some(val);
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Payload operations
    // -----------------------------------------------------------------------

    pub(crate) async fn read_payload_by_id(
        &self,
        payload_id: &str,
    ) -> Result<Option<PayloadRecord>> {
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id = $payload_id LIMIT 1"
        );
        let response = self
            .db
            .query(&query)
            .bind(("payload_id", payload_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(v) = rows.into_iter().next() else {
            return Ok(None);
        };
        let rec = decode_payload_row(v)?;
        Ok(Some(self.hydrate_payload_record(rec).await?))
    }

    pub(crate) async fn read_payloads_by_activity(
        &self,
        activity_id: &str,
    ) -> Result<Vec<PayloadRecord>> {
        let query = format!(
            "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE activity_id = $activity_id ORDER BY payload_kind"
        );
        let response = self
            .db
            .query(&query)
            .bind(("activity_id", activity_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let decoded: Vec<PayloadRecord> = rows
            .into_iter()
            .map(decode_payload_row)
            .collect::<Result<Vec<_>>>()?;
        self.hydrate_payload_records(decoded).await
    }

    /// Payload text search via SurrealDB BM25 full-text index.
    /// Used by `query_ops` to filter rows by payload content.
    pub(crate) async fn search_payload_activity_ids(
        &self,
        query_text: &str,
    ) -> Result<Vec<String>> {
        // Normalize query text for SurrealDB full-text search.
        let normalized = normalize_payload_text_query(query_text);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }

        let query = format!(
            "SELECT DISTINCT activity_id FROM {TBL_PAYLOAD} WHERE {FTS_PAYLOAD_ACTIVITY_WHERE}"
        );
        let response = self
            .db
            .query(&query)
            .bind(("query_text", normalized))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                row.get("activity_id")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect())
    }
}
