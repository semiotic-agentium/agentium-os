// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! [`ProvenanceContextReader`] and [`ProvenanceQueryApi`].

use std::collections::HashMap;

use async_trait::async_trait;
use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceContextMessage, ProvenanceConversationContextItem,
    SessionStepContent, SessionStepOp, ToolCallContent, ToolOutcome, ToolResultContent,
    ToolSessionPhase, conversation_history_role_for_message,
};
use baml_rt_core::{
    Citation,
    ids::{ActivityAnchorId, ContextId, MessageId, TaskId},
    is_history_infrastructure_notice,
};
use serde_json::{Map, Value};

use super::{
    SurrealProvenanceStore,
    conversation_context_pipeline::ConversationContextBatch,
    helpers::{
        check_and_take_zero, has_meaningful_result, is_empty_object,
        json_value_from_embedded_string, map_surreal_error, metadata_error,
        normalize_message_content, parse_json_object_field,
    },
    payload::{decode_payload_row, payload_id_for},
};
use crate::{
    error::Result,
    events::ToolSessionStepOpKind,
    id_semantics::{
        context_entity_id_string, task_entity_id_string_raw, task_execution_activity_id_string,
    },
    payload_id::DEFAULT_SESSION_READ_LINE_LIMIT,
    payload_record::PayloadRecord,
    store::{ProvenanceContextReader, ProvenanceQueryApi},
    surreal_tables::{PAYLOAD_ROW_SELECT, TBL_EDGE, TBL_NODE, TBL_PAYLOAD},
    vocabulary::{a2a_relations, context_scope, semantic_labels},
};

fn is_session_bookkeeping_result(phase: &ToolSessionPhase, value: &Value) -> bool {
    if !phase.is_session_phase() {
        return false;
    }
    match value {
        Value::Object(map) if map.len() == 1 => matches!(
            map.get("status").and_then(Value::as_str),
            Some("sent" | "finished" | "aborted" | "opened")
        ),
        _ => false,
    }
}

#[async_trait]
impl ProvenanceContextReader for SurrealProvenanceStore {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        // SurrealDB requires every `ORDER BY` field to appear in the projection.
        let query = format!(
            "SELECT node_id, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node_id AND rel_type = '{scoped}' AND from_label = 'Message'\
             ) ORDER BY event_order ASC, node_id ASC",
            scoped = context_scope::SCOPED_TO,
        );
        let response = self
            .db
            .query(&query)
            .bind(("ctx_node_id", ctx_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let message_id = props
                .get("a2a_message_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let role = props
                .get("a2a_role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
            let content_value = json_value_from_embedded_string(&content_raw);
            let content = normalize_message_content(&content_value);
            if content.trim().is_empty() || is_history_infrastructure_notice(&content) {
                continue;
            }
            messages.push(ProvenanceContextMessage {
                message_id: MessageId::from(message_id),
                timestamp_ms: props
                    .get("a2a_event_order")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                role: role.to_string(),
                content: vec![content],
            });
        }
        messages.retain(|m| !m.content.iter().all(|c| c.trim().is_empty()));
        messages.sort_by_key(|m| m.timestamp_ms);
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if messages.len() > n {
                let had = messages.len();
                tracing::debug!(
                    %context_id,
                    limit = n,
                    had,
                    "truncating context messages to last N (tail cap)"
                );
                messages = messages.split_off(messages.len() - n);
            }
        }
        Ok(messages)
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(context_id, limit, None, None, false)
            .await
    }

    async fn conversation_context_with_task(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(context_id, limit, task_id, None, false)
            .await
    }
}

fn warn_conversation_context_row_skip(context_id: &ContextId, row: &Value, reason: &'static str) {
    tracing::warn!(
        target: "baml_rt_provenance::conversation_context",
        context_id = %context_id.as_str(),
        node_id = row.get("node_id").and_then(serde_json::Value::as_str).unwrap_or(""),
        label = row.get("label").and_then(serde_json::Value::as_str).unwrap_or(""),
        reason,
        "skipping graph row for conversation_context export"
    );
}

impl SurrealProvenanceStore {
    pub(super) async fn conversation_context_filtered(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        after_event_order: Option<u64>,
        forward_limit: bool,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        let scoped_to = context_scope::SCOPED_TO;

        let task_filter_sql = match task_id {
            None => String::new(),
            Some(_) => {
                let tm = a2a_relations::TASK_MESSAGE;
                let tc = a2a_relations::TASK_CALL;
                let ts = a2a_relations::TASK_SESSION_STEP;
                format!(
                    "AND (\
                       (label = 'Message' AND node_id IN (\
                         SELECT VALUE to_id FROM {TBL_EDGE} \
                         WHERE from_id = $task_entity_id AND rel_type = '{tm}'\
                       ))\
                       OR (label = 'ToolCall' AND node_id IN (\
                         SELECT VALUE to_id FROM {TBL_EDGE} \
                         WHERE from_id = $task_exec_id AND rel_type = '{tc}'\
                       ))\
                       OR (label = 'SessionStep' AND node_id IN (\
                         SELECT VALUE to_id FROM {TBL_EDGE} \
                         WHERE from_id = $task_entity_id AND rel_type = '{ts}'\
                       ))\
                     )"
                )
            }
        };

        let after_filter_sql = match after_event_order {
            Some(_) => "AND props.a2a_event_order > $after_event_order",
            None => "",
        };

        // Single SCOPED_TO edge traversal: fetch all Message, ToolCall, SessionStep
        // nodes scoped to this context in one query.
        let main_query = format!(
            "SELECT node_id, label, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node_id AND rel_type = '{scoped_to}' \
                 AND from_label IN ['Message', 'ToolCall', 'SessionStep']\
             ) \
             AND (label != 'ToolCall' OR props.a2a_activity_outcome IN ['Success', 'Failed']) \
             {after_filter_sql} \
             {task_filter_sql} \
             ORDER BY event_order ASC, node_id ASC"
        );

        let mut q = self.db.query(&main_query);
        q = q.bind(("ctx_node_id", ctx_node_id.clone()));
        if let Some(tid) = task_id {
            q = q.bind(("task_entity_id", task_entity_id_string_raw(tid.as_str())));
            q = q.bind((
                "task_exec_id",
                task_execution_activity_id_string(tid.as_str()),
            ));
        }
        if let Some(after) = after_event_order {
            q = q.bind(("after_event_order", after));
        }
        let response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        // Collect ToolCall node_ids, payload anchors, and Message node_ids for batch queries.
        let mut tool_call_node_ids: Vec<String> = Vec::new();
        let mut payload_ids: Vec<String> = Vec::new();
        let mut message_node_ids: Vec<String> = Vec::new();
        for row in &rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            if label == "ToolCall" {
                if let Some(nid) = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    tool_call_node_ids.push(nid.to_string());
                }
                if let Some(anchor) = row
                    .get("props")
                    .and_then(|p| p.get("a2a_activity_anchor"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    payload_ids.push(payload_id_for(anchor, "tool_call"));
                    payload_ids.push(payload_id_for(anchor, "tool_result"));
                }
            } else if label == "Message"
                && let Some(nid) = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                message_node_ids.push(nid.to_string());
            }
        }

        // Single batch payload fetch for all ToolCall payloads (tool_call + tool_result).
        let payload_map: HashMap<String, PayloadRecord> = if payload_ids.is_empty() {
            HashMap::new()
        } else {
            let in_list = payload_ids
                .iter()
                .map(|id| format!("\'{id}\'"))
                .collect::<Vec<_>>()
                .join(", ");
            let pq = format!(
                "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id IN [{in_list}]"
            );
            let prows: Vec<Value> = self.query_sql_rows(&pq).await?;
            let mut map = HashMap::new();
            for v in prows {
                let rec = decode_payload_row(v)?;
                let rec = self.hydrate_payload_record(rec).await?;
                map.insert(rec.payload_id.clone(), rec);
            }
            map
        };

        // Scoped WAS_USED_BY -> ToolArgs edge validation (only for our ToolCalls).
        let tool_call_edge_info: HashMap<String, (String, String)> =
            if tool_call_node_ids.is_empty() {
                HashMap::new()
            } else {
                let node_id_list = tool_call_node_ids
                    .iter()
                    .map(|id| format!("\'{id}\'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let eq = format!(
                    "SELECT from_id, props OMIT id FROM {TBL_EDGE} \
                     WHERE rel_type = '{}' AND from_label = 'ToolCall' AND to_label = 'ToolArgs' \
                       AND from_id IN [{node_id_list}]",
                    semantic_labels::WAS_USED_BY
                );
                let erows: Vec<Value> = self.query_sql_rows(&eq).await?;
                let mut info = HashMap::new();
                for edge in &erows {
                    let from_id = edge
                        .get("from_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let edge_props = edge.get("props").and_then(Value::as_object);
                    let prov_role = edge_props
                        .and_then(|p| p.get("prov_role"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let prov_type = edge_props
                        .and_then(|p| p.get("prov_type"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    info.insert(from_id.to_string(), (prov_role, prov_type));
                }
                info
            };

        // Batch-fetch CITED edges for all Message nodes to get their citation strings.
        // The CITED edges written by the normalizer carry a `raw` attribute with the
        // original citation string (#N, @N, …).
        let message_citations_map: HashMap<String, Vec<Citation>> = if message_node_ids.is_empty() {
            HashMap::new()
        } else {
            let node_id_list = message_node_ids
                .iter()
                .map(|id| format!("\'{id}\'"))
                .collect::<Vec<_>>()
                .join(", ");
            let cited_rel = semantic_labels::CITED;
            let cq = format!(
                "SELECT from_id, props.raw AS raw FROM {TBL_EDGE} \
                     WHERE rel_type = '{cited_rel}' AND from_label = 'Message' \
                       AND from_id IN [{node_id_list}]"
            );
            let crows: Vec<Value> = self.query_sql_rows(&cq).await?;
            let mut map: HashMap<String, Vec<Citation>> = HashMap::new();
            for edge in &crows {
                let from_id = edge
                    .get("from_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if from_id.is_empty() {
                    continue;
                }
                if let Some(raw) = edge
                    .get("raw")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    && let Ok(c) = Citation::try_new(raw)
                {
                    map.entry(from_id.to_string()).or_default().push(c);
                }
            }
            map
        };

        // Process the unified result set, discriminating by label.
        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in &rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            let props = match row.get("props") {
                Some(p) => p,
                None => {
                    warn_conversation_context_row_skip(context_id, row, "missing_props");
                    continue;
                }
            };

            match label {
                "Message" => {
                    let node_id = row
                        .get("node_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                        Some(id) if !id.is_empty() => id,
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "message_missing_activity_anchor",
                            );
                            continue;
                        }
                    };
                    let role = props
                        .get("a2a_role")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
                    let content_value = json_value_from_embedded_string(&content_raw);
                    let text = normalize_message_content(&content_value);
                    if text.trim().is_empty() {
                        warn_conversation_context_row_skip(context_id, row, "message_empty_text");
                        continue;
                    }
                    // Look up CITED edges for this Message node to get citations.
                    let citations = message_citations_map
                        .get(node_id)
                        .cloned()
                        .unwrap_or_default();
                    items.push(ProvenanceConversationContextItem {
                        timestamp_ms: props
                            .get("a2a_event_order")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        activity_anchor: ActivityAnchorId::from(event_id),
                        role: conversation_history_role_for_message(role),
                        content: ConversationItemContent::Message { text, citations },
                    });
                }
                "ToolCall" => {
                    let node_id = row
                        .get("node_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let event_id_str =
                        match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                            Some(id) if !id.is_empty() => id,
                            _ => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "tool_call_missing_activity_anchor",
                                );
                                continue;
                            }
                        };
                    let tool_name = match props.get("a2a_tool_name").and_then(Value::as_str) {
                        Some(name) if !name.is_empty() => name.to_string(),
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "tool_call_missing_tool_name",
                            );
                            continue;
                        }
                    };

                    // When present, enforce ToolCall→ToolArgs WAS_USED_BY topology. A missing edge does
                    // not invalidate the row — some writers attach arguments only via payloads or node
                    // metadata without a separate ToolArgs vertex.
                    if let Some((prov_role, prov_type)) = tool_call_edge_info.get(node_id) {
                        let role_ok = prov_role.is_empty() || prov_role == "a2a:args";
                        let type_ok = prov_type.is_empty() || prov_type == "a2a:ToolArgs";
                        if !role_ok || !type_ok {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "tool_call_invalid_tool_args_edge",
                            );
                            continue;
                        }
                    }

                    let metadata: Value = props
                        .get("a2a_metadata")
                        .and_then(parse_json_object_field)
                        .unwrap_or(Value::Object(Map::new()));
                    let metadata_args = metadata
                        .get("args")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new()));

                    let tool_call_payload =
                        payload_map.get(&payload_id_for(event_id_str, "tool_call"));
                    let tool_result_payload =
                        payload_map.get(&payload_id_for(event_id_str, "tool_result"));

                    let (args, phase) = if let Some(payload) = tool_call_payload {
                        let parsed: Value =
                            serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                        let args = parsed
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| metadata_args.clone());
                        let phase_label = parsed
                            .get("phase")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default();
                        (
                            args,
                            ToolSessionPhase::from_metadata(
                                &serde_json::json!({ "phase": phase_label }),
                            ),
                        )
                    } else {
                        (metadata_args, ToolSessionPhase::from_metadata(&metadata))
                    };

                    let (result, error) = if let Some(payload) = tool_result_payload {
                        let parsed: Value =
                            serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                        let result = parsed
                            .get("result")
                            .cloned()
                            .unwrap_or_else(|| parsed.clone());
                        let error = parsed.get("error").cloned();
                        (result, error)
                    } else {
                        let result = metadata
                            .get("result")
                            .cloned()
                            .unwrap_or(Value::Object(Map::new()));
                        let error = metadata_error(&metadata);
                        (result, error)
                    };

                    let has_outcome = (has_meaningful_result(&result)
                        && !is_session_bookkeeping_result(&phase, &result))
                        || error.is_some();
                    // Non-session (execute/unknown) invocations always surface so the UI can render
                    // tool cards even when args/results are empty. Session FSM phases are usually
                    // narrated by SessionStep rows; still emit ToolCall pairs when the send carries
                    // args or a terminal outcome so host tools (e.g. session Send with LLM payload)
                    // remain visible if session-step projection is incomplete.
                    let include_call =
                        !phase.is_session_phase() || !is_empty_object(&args) || has_outcome;

                    let tool_event_order = props
                        .get("a2a_event_order")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);

                    if include_call {
                        items.push(ProvenanceConversationContextItem {
                            timestamp_ms: tool_event_order,
                            activity_anchor: ActivityAnchorId::from(event_id_str),
                            role: "tool".to_string(),
                            content: ConversationItemContent::ToolCall(ToolCallContent {
                                tool_name: tool_name.clone(),
                                args,
                                fsm_phase: phase.clone(),
                            }),
                        });

                        let outcome = if let Some(error) = error {
                            ToolOutcome::Error(error)
                        } else if has_meaningful_result(&result)
                            && !is_session_bookkeeping_result(&phase, &result)
                        {
                            ToolOutcome::Result(result)
                        } else {
                            ToolOutcome::StatusOnly
                        };
                        items.push(ProvenanceConversationContextItem {
                            timestamp_ms: tool_event_order,
                            activity_anchor: ActivityAnchorId::from(event_id_str),
                            role: "tool".to_string(),
                            content: ConversationItemContent::ToolResult(ToolResultContent {
                                tool_name,
                                fsm_phase: phase,
                                outcome,
                            }),
                        });
                    }
                }
                "SessionStep" => {
                    let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                        Some(id) if !id.is_empty() => id.to_string(),
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "session_step_missing_activity_anchor",
                            );
                            continue;
                        }
                    };
                    let tool_name = props
                        .get("a2a_tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let op_kind_raw = props
                        .get("op_kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Some(op_kind) = ToolSessionStepOpKind::parse_graph(op_kind_raw) else {
                        warn_conversation_context_row_skip(
                            context_id,
                            row,
                            "session_step_unknown_op_kind",
                        );
                        continue;
                    };
                    let header = props
                        .get("header")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let archive_ref = props
                        .get("archive_ref")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let grep = props
                        .get("grep")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);

                    let op = match op_kind {
                        ToolSessionStepOpKind::Open => SessionStepOp::Open,
                        ToolSessionStepOpKind::SendDone => match (archive_ref, header) {
                            (Some(r), Some(hdr)) => {
                                let informed_by = props
                                    .get("informed_by_tool_activity_anchor")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                SessionStepOp::SendDone {
                                    archive_ref: r,
                                    header: hdr,
                                    informed_by,
                                }
                            }
                            _ => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_send_done_incomplete",
                                );
                                continue;
                            }
                        },
                        ToolSessionStepOpKind::SearchRead => match archive_ref {
                            Some(r) => {
                                let offset =
                                    props.get("offset").and_then(Value::as_u64).unwrap_or(0)
                                        as usize;
                                let limit = props
                                    .get("limit")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(DEFAULT_SESSION_READ_LINE_LIMIT as u64)
                                    as usize;
                                let Some(grep_pat) = grep else {
                                    warn_conversation_context_row_skip(
                                        context_id,
                                        row,
                                        "session_step_search_read_missing_grep",
                                    );
                                    continue;
                                };
                                SessionStepOp::SearchRead {
                                    archive_ref: r,
                                    grep: grep_pat,
                                    offset,
                                    limit,
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_search_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                        ToolSessionStepOpKind::PageRead => match archive_ref {
                            Some(r) => {
                                let offset =
                                    props.get("offset").and_then(Value::as_u64).unwrap_or(0)
                                        as usize;
                                let limit = props
                                    .get("limit")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(DEFAULT_SESSION_READ_LINE_LIMIT as u64)
                                    as usize;
                                SessionStepOp::PageRead {
                                    archive_ref: r,
                                    offset,
                                    limit,
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_page_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                        // Legacy graphs from before SearchRead/PageRead split.
                        ToolSessionStepOpKind::Read => match archive_ref {
                            Some(r) => {
                                if let Some(g) = grep.filter(|s| !s.is_empty()) {
                                    SessionStepOp::SearchRead {
                                        archive_ref: r,
                                        grep: g,
                                        offset: 0,
                                        limit: DEFAULT_SESSION_READ_LINE_LIMIT,
                                    }
                                } else {
                                    SessionStepOp::PageRead {
                                        archive_ref: r,
                                        offset: 0,
                                        limit: DEFAULT_SESSION_READ_LINE_LIMIT,
                                    }
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_legacy_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                    };

                    items.push(ProvenanceConversationContextItem {
                        timestamp_ms: props
                            .get("a2a_event_order")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        activity_anchor: ActivityAnchorId::from(event_id.as_str()),
                        role: "tool".to_string(),
                        content: ConversationItemContent::SessionStep(SessionStepContent {
                            tool_name,
                            op,
                            send_done_replay_payload: None,
                            read_replay_lines: None,
                        }),
                    });
                }
                _ => {
                    if !label.is_empty() {
                        warn_conversation_context_row_skip(
                            context_id,
                            row,
                            "unsupported_or_unknown_label",
                        );
                    }
                    continue;
                }
            }
        }

        let mut items = ConversationContextBatch::from_graph_rows(items)
            .hydrate(self)
            .await?
            .canonicalize_suppress_covered_tool_rows()
            .into_items();

        items.sort_by_key(|i| i.timestamp_ms);
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if items.len() > n {
                let had = items.len();
                tracing::debug!(
                    %context_id,
                    limit = n,
                    had,
                    forward = forward_limit,
                    "truncating conversation context to limit (after sort)"
                );
                if forward_limit {
                    items.truncate(n);
                } else {
                    items = items.split_off(items.len() - n);
                }
            }
        }
        Ok(items)
    }
}

#[async_trait]
impl ProvenanceQueryApi for SurrealProvenanceStore {
    async fn query_context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        ProvenanceContextReader::context_messages(self, context_id, limit).await
    }

    async fn query_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(context_id, limit, task_id, None, false)
            .await
    }

    async fn query_conversation_context_after(
        &self,
        context_id: &ContextId,
        after_event_order: u64,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(
            context_id,
            limit,
            task_id,
            Some(after_event_order),
            true,
        )
        .await
    }
}
