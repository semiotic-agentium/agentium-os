#![expect(

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

    clippy::result_large_err,
    reason = "ToolSessionError is large by design; matches the session-tool patterns"
)]

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_conversation::{
    projection::agent_view_projection_item,
    view::{AgentRef, ProvenanceConversationContextItem},
};
use baml_rt_core::{BamlRtError, Result};
use baml_rt_provenance::{
    ContextHistoryCursor, ProvenanceArchivePayload, ProvenanceArchiveRecord, ProvenanceOpsFilters,
    ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource, ProvenanceOutcomeSegment,
    ProvenanceResponseProfile,
};
use baml_rt_tools::{
    OpaqueJson, ToolFailure, ToolHandler, ToolSession, ToolSessionError, ToolStep,
    opaque_json_map_from_object,
    prompt_projection::{PromptProjectionContent, PromptProjectionItem, SessionStepProjection},
    tools::{
        HistoryContextSessionOp, HistoryContextStatus, HistoryContextV1, SessionReadMode,
        ToolFunctionMetadata, ToolSessionContext,
    },
};
use serde_json::{Value, json};

use crate::{
    metadata::{system_extrospection_metadata, system_introspection_metadata},
    tools::{
        ProvenanceAgentRefDto, ProvenanceArchivePayloadDto, ProvenanceArchiveRecordDto,
        ProvenanceArchiveSummaryDto, ProvenanceContextHistoryPageDto,
        ProvenanceContextHistoryRowDto, ProvenancePayloadLlmCallDto, ProvenancePayloadLlmResultDto,
        ProvenancePayloadToolCallDto, ProvenancePayloadToolResultDto, ProvenanceQueryNextOutput,
        ProvenanceQueryOpenInput, ProvenanceQuerySendInput, ProvenanceReadProjectionDto,
        ProvenanceRetrievalBudgetDto, SessionReadResultDto,
    },
};

const RETRIEVAL_CALLS_CAP: u32 = 8;
/// Bound on store pages skipped because the window was chosen before dedup
/// elided its rows. Realistic contexts need one; the bound keeps a pathological
/// run of elided-only windows from spinning.
const CONTEXT_HISTORY_MAX_EMPTY_PAGES: u32 = 8;
const RETRIEVAL_BYTES_CAP: u32 = 64 * 1024;
const RETRIEVAL_ITEMS_CAP: u32 = 64;

#[derive(Debug, Clone, Copy)]
enum ProvenanceReadProjection {
    Identity,
    Summary,
    Detail,
}

impl ProvenanceReadProjection {
    fn parse(raw: Option<&str>) -> std::result::Result<Self, ToolSessionError> {
        match raw
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("summary") => Ok(Self::Summary),
            Some("identity") => Ok(Self::Identity),
            Some("detail") => Ok(Self::Detail),
            Some(other) => Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "unsupported read.projection '{other}' for provenance session tool"
            )))),
        }
    }

    fn to_dto(self) -> ProvenanceReadProjectionDto {
        match self {
            Self::Identity => ProvenanceReadProjectionDto::Identity,
            Self::Summary => ProvenanceReadProjectionDto::Summary,
            Self::Detail => ProvenanceReadProjectionDto::Detail,
        }
    }
}

fn archive_payload_to_dto(payload: ProvenanceArchivePayload) -> ProvenanceArchivePayloadDto {
    match payload {
        ProvenanceArchivePayload::LlmCall {
            payload_ref,
            activity_ref,
            prompt_json,
        } => ProvenanceArchivePayloadDto::LlmCall(ProvenancePayloadLlmCallDto {
            payload_ref: payload_ref.0,
            activity_ref: activity_ref.0,
            prompt_json,
        }),
        ProvenanceArchivePayload::LlmResult {
            payload_ref,
            activity_ref,
            result_json,
        } => ProvenanceArchivePayloadDto::LlmResult(ProvenancePayloadLlmResultDto {
            payload_ref: payload_ref.0,
            activity_ref: activity_ref.0,
            result_json,
        }),
        ProvenanceArchivePayload::ToolCall {
            payload_ref,
            activity_ref,
            tool_name,
            phase,
            args_json,
        } => ProvenanceArchivePayloadDto::ToolCall(ProvenancePayloadToolCallDto {
            payload_ref: payload_ref.0,
            activity_ref: activity_ref.0,
            tool_name,
            phase,
            args_json,
        }),
        ProvenanceArchivePayload::ToolResult {
            payload_ref,
            activity_ref,
            result_json,
        } => ProvenanceArchivePayloadDto::ToolResult(ProvenancePayloadToolResultDto {
            payload_ref: payload_ref.0,
            activity_ref: activity_ref.0,
            result_json,
        }),
    }
}

fn archive_record_to_dto(record: ProvenanceArchiveRecord) -> ProvenanceArchiveRecordDto {
    ProvenanceArchiveRecordDto {
        archive_ref: record.archive_ref.0,
        payloads: record
            .payloads
            .into_iter()
            .map(archive_payload_to_dto)
            .collect(),
    }
}

fn payload_source_name(payload: &ProvenanceArchivePayloadDto) -> &'static str {
    match payload {
        ProvenanceArchivePayloadDto::LlmCall(_) => "llm_call",
        ProvenanceArchivePayloadDto::LlmResult(_) => "llm_result",
        ProvenanceArchivePayloadDto::ToolCall(_) => "tool_call",
        ProvenanceArchivePayloadDto::ToolResult(_) => "tool_result",
    }
}

fn collect_refs_with_prefix(
    prefix_ref: &str,
    record: Option<&ProvenanceArchiveRecordDto>,
) -> Vec<String> {
    let mut refs = vec![prefix_ref.to_string()];
    let Some(record) = record else {
        return refs;
    };

    refs.push(record.archive_ref.clone());
    for payload in &record.payloads {
        match payload {
            ProvenanceArchivePayloadDto::LlmCall(ProvenancePayloadLlmCallDto {
                payload_ref,
                activity_ref,
                ..
            })
            | ProvenanceArchivePayloadDto::LlmResult(ProvenancePayloadLlmResultDto {
                payload_ref,
                activity_ref,
                ..
            })
            | ProvenanceArchivePayloadDto::ToolResult(ProvenancePayloadToolResultDto {
                payload_ref,
                activity_ref,
                ..
            }) => {
                refs.push(payload_ref.clone());
                refs.push(activity_ref.clone());
            }
            ProvenanceArchivePayloadDto::ToolCall(ProvenancePayloadToolCallDto {
                payload_ref,
                activity_ref,
                ..
            }) => {
                refs.push(payload_ref.clone());
                refs.push(activity_ref.clone());
            }
        }
    }
    refs.sort();
    refs.dedup();
    if refs.first().map(|s| s.as_str()) != Some(prefix_ref) {
        refs.retain(|r| r != prefix_ref);
        refs.insert(0, prefix_ref.to_string());
    }
    refs
}

fn summarize_record(record: &ProvenanceArchiveRecordDto) -> ProvenanceArchiveSummaryDto {
    let payload_sources = record
        .payloads
        .iter()
        .map(payload_source_name)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    ProvenanceArchiveSummaryDto {
        archive_ref: record.archive_ref.clone(),
        payload_count: u32::try_from(record.payloads.len()).unwrap_or(u32::MAX),
        payload_sources,
    }
}

/// Resource selected by one retrieval `Send`.
///
/// Ops resources return provenance records and payload DTOs. `context_history` returns the
/// prompt-facing view rows instead, because those are the rows the model was shown and the
/// only rows it may page back in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProvenanceQueryResource {
    Ops(ProvenanceOpsResource),
    ContextHistory,
}

fn parse_resource(raw: &str) -> ProvenanceQueryResource {
    match raw.trim().to_ascii_lowercase().as_str() {
        "context_history" => ProvenanceQueryResource::ContextHistory,
        "tool_calls" => ProvenanceQueryResource::Ops(ProvenanceOpsResource::ToolCalls),
        "messages" => ProvenanceQueryResource::Ops(ProvenanceOpsResource::Messages),
        "aggregates" => ProvenanceQueryResource::Ops(ProvenanceOpsResource::Aggregates),
        _ => ProvenanceQueryResource::Ops(ProvenanceOpsResource::LlmCalls),
    }
}

/// Scope-bound cursor prefix. The store cursor only orders rows; binding it to
/// the read scope is what lets the tool reject a cursor minted for another
/// context or agent instead of silently paging a foreign window.
const CONTEXT_HISTORY_CURSOR_PREFIX: &str = "ch1";

fn encode_context_history_cursor(
    context_id: &baml_rt_core::ids::ContextId,
    agent_id: &baml_rt_core::ids::AgentId,
    inner: &str,
) -> String {
    format!(
        "{CONTEXT_HISTORY_CURSOR_PREFIX}:{}:{}:{}{}{inner}",
        context_id.as_str().len(),
        agent_id.as_str().len(),
        context_id.as_str(),
        agent_id.as_str(),
    )
}

fn malformed_cursor(reason: &str) -> ToolSessionError {
    ToolSessionError::Tool(ToolFailure::invalid_input(format!(
        "invalid context_history cursor ({reason}); omit the cursor to start again from the newest window"
    )))
}

fn decode_context_history_cursor(
    raw: &str,
    context_id: &baml_rt_core::ids::ContextId,
    agent_id: &baml_rt_core::ids::AgentId,
) -> std::result::Result<ContextHistoryCursor, ToolSessionError> {
    let Some(rest) = raw.strip_prefix(CONTEXT_HISTORY_CURSOR_PREFIX) else {
        return Err(malformed_cursor("unrecognized cursor version"));
    };
    let Some(rest) = rest.strip_prefix(':') else {
        return Err(malformed_cursor("missing context length"));
    };
    let (ctx_len, rest) = rest
        .split_once(':')
        .ok_or_else(|| malformed_cursor("missing agent length"))?;
    let (agent_len, rest) = rest
        .split_once(':')
        .ok_or_else(|| malformed_cursor("missing cursor body"))?;
    let ctx_len = ctx_len
        .parse::<usize>()
        .map_err(|_| malformed_cursor("non-numeric context length"))?;
    let agent_len = agent_len
        .parse::<usize>()
        .map_err(|_| malformed_cursor("non-numeric agent length"))?;
    let (bound_context, rest) = rest
        .split_at_checked(ctx_len)
        .ok_or_else(|| malformed_cursor("truncated context binding"))?;
    let (bound_agent, inner) = rest
        .split_at_checked(agent_len)
        .ok_or_else(|| malformed_cursor("truncated agent binding"))?;
    if bound_context != context_id.as_str() || bound_agent != agent_id.as_str() {
        return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
            "stale context_history cursor: it was minted for a different context or agent; \
             omit the cursor to start again from the newest window"
                .to_string(),
        )));
    }
    ContextHistoryCursor::decode(inner).ok_or_else(|| malformed_cursor("undecodable ordering key"))
}

fn agent_ref_dto(reference: &AgentRef) -> ProvenanceAgentRefDto {
    ProvenanceAgentRefDto {
        agent_id: reference.agent_id.as_str().to_string(),
        name: reference.name.clone(),
    }
}

/// Serialize the prompt projection of one row without its replay-only payloads.
fn projection_content_to_value(content: PromptProjectionContent) -> Value {
    match content {
        PromptProjectionContent::Message { text, citations } => json!({
            "kind": "message",
            "text": text,
            "citations": citations,
        }),
        PromptProjectionContent::ToolCall { tool_name, args } => json!({
            "kind": "tool_call",
            "tool_name": tool_name,
            "args": args,
        }),
        PromptProjectionContent::ToolResult { tool_name, result } => json!({
            "kind": "tool_result",
            "tool_name": tool_name,
            "result": result,
        }),
        PromptProjectionContent::ToolError { tool_name, error } => json!({
            "kind": "tool_error",
            "tool_name": tool_name,
            "error": error,
        }),
        PromptProjectionContent::SessionStep(step) => {
            let mut value = json!({
                "kind": "session_step",
                "tool_name": step.tool_name,
            });
            let op = match step.op {
                SessionStepProjection::Open => json!({ "op": "open" }),
                SessionStepProjection::SendDone {
                    archive_ref,
                    header,
                } => json!({
                    "op": "send_done",
                    "archive_ref": archive_ref,
                    "header": header,
                }),
                SessionStepProjection::SearchRead {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                } => json!({
                    "op": "search_read",
                    "archive_ref": archive_ref,
                    "grep": grep,
                    "offset": offset,
                    "limit": limit,
                }),
                SessionStepProjection::PageRead {
                    archive_ref,
                    offset,
                    limit,
                } => json!({
                    "op": "page_read",
                    "archive_ref": archive_ref,
                    "offset": offset,
                    "limit": limit,
                }),
            };
            if let (Some(target), Some(source)) = (value.as_object_mut(), op.as_object()) {
                for (key, item) in source {
                    target.insert(key.clone(), item.clone());
                }
            }
            value
        }
        PromptProjectionContent::Planning(plan) => json!({
            "kind": "planning",
            "event_kind": plan.kind.as_wire_str(),
            "summary": plan.summary,
            "detail": plan.detail,
            "intent_id": plan.intent_id,
            "plan_id": plan.plan_id,
            "step_id": plan.step_id,
            "old_status": plan.old_status,
            "new_status": plan.new_status,
        }),
        PromptProjectionContent::CompactionSummary {
            summary,
            covered_event_order_start,
            covered_event_order_end,
        } => json!({
            "kind": "compaction_summary",
            "summary": summary,
            "covered_event_order_start": covered_event_order_start,
            "covered_event_order_end": covered_event_order_end,
        }),
        PromptProjectionContent::Notice { text } => json!({
            "kind": "notice",
            "text": text,
        }),
    }
}

/// Project one stored row exactly as the prompt does, then carry the row's
/// attribution alongside so a paged row is as interpretable as a streamed one.
fn context_history_row(
    item: ProvenanceConversationContextItem,
    reader_agent_id: &baml_rt_core::ids::AgentId,
) -> Option<ProvenanceContextHistoryRowDto> {
    let attribution = item.attribution.clone();
    let projected = agent_view_projection_item(item, reader_agent_id)?;
    let PromptProjectionItem {
        timestamp_ms,
        activity_anchor,
        role,
        content,
    } = projected;
    let attribution = attribution.as_deref();
    Some(ProvenanceContextHistoryRowDto {
        activity_anchor,
        role,
        timestamp_ms,
        event_kind: attribution.and_then(|a| {
            serde_json::to_value(a.event_kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        }),
        actor: attribution
            .and_then(|a| a.actor.as_ref())
            .map(agent_ref_dto),
        target: attribution
            .and_then(|a| a.target.as_ref())
            .map(agent_ref_dto),
        task_id: attribution
            .and_then(|a| a.task_id.as_ref())
            .map(|t| t.as_str().to_string()),
        source_identity: attribution.and_then(|a| a.source_identity.clone()),
        content: OpaqueJson::from(projection_content_to_value(content)),
    })
}

fn parse_outcome(raw: Option<&str>) -> ProvenanceOutcomeSegment {
    match raw.unwrap_or("both").to_ascii_lowercase().as_str() {
        "failed_only" => ProvenanceOutcomeSegment::FailedOnly,
        "successful_only" => ProvenanceOutcomeSegment::SuccessfulOnly,
        _ => ProvenanceOutcomeSegment::Both,
    }
}

struct ProvenanceQuerySession {
    ctx: ToolSessionContext,
    scoped_to_context: bool,
    query: Arc<dyn ProvenanceOpsQuery>,
    pending: Option<Value>,
    retrieval_calls_used: u32,
    retrieval_bytes_used: u32,
    retrieval_items_used: u32,
    read_hop: u32,
}

impl ProvenanceQuerySession {
    fn retrieval_budget(&self) -> ProvenanceRetrievalBudgetDto {
        ProvenanceRetrievalBudgetDto {
            calls_used: self.retrieval_calls_used,
            calls_cap: RETRIEVAL_CALLS_CAP,
            bytes_used: self.retrieval_bytes_used,
            bytes_cap: RETRIEVAL_BYTES_CAP,
            items_used: self.retrieval_items_used,
            items_cap: RETRIEVAL_ITEMS_CAP,
        }
    }

    fn budget_exhausted_output(&self) -> ProvenanceQueryNextOutput {
        ProvenanceQueryNextOutput {
            payload_json: None,
            read_result: None,
            context_history: None,
            retrieval_budget: Some(self.retrieval_budget()),
            budget_exhausted: Some(true),
            done: true,
            history_context: None,
        }
    }

    /// Page one window of the prompt-facing context history view.
    ///
    /// The view is always read across tasks because that is what the prompt does; a task
    /// filter here would hide rows the prompt showed. The existing retrieval budget is the
    /// ceiling: one page may not exceed the items or bytes left in it.
    async fn send_context_history(
        &mut self,
        send: &ProvenanceQuerySendInput,
    ) -> std::result::Result<(), ToolSessionError> {
        if self.retrieval_calls_used >= RETRIEVAL_CALLS_CAP
            || self.retrieval_bytes_used >= RETRIEVAL_BYTES_CAP
            || self.retrieval_items_used >= RETRIEVAL_ITEMS_CAP
        {
            self.pending = Some(
                serde_json::to_value(self.budget_exhausted_output()).map_err(|e| {
                    ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
                })?,
            );
            return Ok(());
        }

        // Context and agent come from the host-bound scope, never from model input.
        let context_id = if self.scoped_to_context {
            self.ctx.context_id.clone()
        } else {
            send.context_id
                .clone()
                .unwrap_or_else(|| self.ctx.context_id.clone())
        };
        let agent_id = if self.scoped_to_context {
            self.ctx.agent_id.clone()
        } else {
            send.agent_id
                .clone()
                .unwrap_or_else(|| self.ctx.agent_id.clone())
        };
        // A malformed or foreign cursor is rejected before the read. The store
        // cannot distinguish an unknown context from a known context with no
        // visible rows, so an empty first page stays the terminal answer; a
        // cursor that does not belong to this scope is the stale case we can
        // prove, and it must never collapse into that empty page.
        let cursor = match send
            .cursor
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
        {
            Some(raw) => Some(decode_context_history_cursor(raw, &context_id, &agent_id)?),
            None => None,
        };

        let remaining_items = RETRIEVAL_ITEMS_CAP
            .saturating_sub(self.retrieval_items_used)
            .max(1);
        let requested = send.page_size.unwrap_or(remaining_items).max(1);
        let mut window = requested.min(remaining_items) as usize;
        let remaining_bytes = RETRIEVAL_BYTES_CAP.saturating_sub(self.retrieval_bytes_used);
        let mut fetch_cursor = cursor;
        let mut skipped_empty_pages = 0u32;

        loop {
            let fetched = self
                .query
                .context_history_view(
                    &context_id,
                    &agent_id,
                    Some(window),
                    fetch_cursor.as_ref(),
                    None,
                )
                .await
                .map_err(|e| {
                    ToolSessionError::Tool(ToolFailure::from_error(
                        &BamlRtError::ProvenanceContextRead {
                            source: Box::new(e),
                        },
                    ))
                })?;
            let has_more = fetched.truncated;
            let fetched_cursor = fetched.cursor;
            let items = fetched
                .items
                .into_iter()
                .filter_map(|item| context_history_row(item, &agent_id))
                .collect::<Vec<_>>();
            // The store selects the window before dedup elides rows, so a
            // window can project to nothing while older rows remain. Advance
            // until the page says something or the history is exhausted, so an
            // empty `items` array only ever means "nothing older to read".
            if items.is_empty()
                && has_more
                && skipped_empty_pages < CONTEXT_HISTORY_MAX_EMPTY_PAGES
                && let Some(next) = fetched_cursor
                    .as_deref()
                    .and_then(ContextHistoryCursor::decode)
            {
                fetch_cursor = Some(next);
                skipped_empty_pages = skipped_empty_pages.saturating_add(1);
                continue;
            }
            let next_cursor = fetched_cursor
                .map(|inner| encode_context_history_cursor(&context_id, &agent_id, &inner));
            let page = ProvenanceContextHistoryPageDto {
                items,
                cursor: next_cursor,
                has_more,
            };
            let bytes = serde_json::to_vec(&page)
                .map(|buf| u32::try_from(buf.len()).unwrap_or(u32::MAX))
                .unwrap_or(u32::MAX);
            // Half the window until the page fits the remaining byte budget. Re-reading from
            // the same cursor can only shrink the same newest tail, so no row is skipped.
            if bytes > remaining_bytes && window > 1 {
                window = (window / 2).max(1);
                continue;
            }
            self.retrieval_calls_used = self.retrieval_calls_used.saturating_add(1);
            self.retrieval_items_used = self
                .retrieval_items_used
                .saturating_add(u32::try_from(page.items.len()).unwrap_or(u32::MAX));
            self.retrieval_bytes_used = self.retrieval_bytes_used.saturating_add(bytes);
            let output = ProvenanceQueryNextOutput {
                payload_json: None,
                read_result: None,
                context_history: Some(page),
                retrieval_budget: Some(self.retrieval_budget()),
                budget_exhausted: Some(false),
                done: true,
                history_context: None,
            };
            self.pending = Some(serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);
            return Ok(());
        }
    }
}

fn history_payload_from_provenance_output(
    output: &serde_json::Map<String, Value>,
) -> Option<BTreeMap<String, OpaqueJson>> {
    if let Some(read_result) = output.get("readResult").and_then(Value::as_object) {
        let mut payload = BTreeMap::new();
        for (k, v) in read_result {
            if k == "refs" || k == "refId" || k == "ref_id" || k == "mode" {
                continue;
            }
            payload.insert(k.clone(), OpaqueJson::from(v.clone()));
        }
        return Some(payload);
    }
    output
        .get("payloadJson")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(opaque_json_map_from_object)
}

fn attach_history_context(value: &mut Value, hop: u32) {
    let Some(output) = value.as_object_mut() else {
        return;
    };
    if output.get("historyContext").is_some() {
        return;
    }
    let context_history_page = output.get("contextHistory").and_then(Value::as_object);
    let payload = history_payload_from_provenance_output(output);
    let cursor = context_history_page
        .and_then(|page| page.get("cursor"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|obj| obj.get("cursor"))
                .and_then(|value| value.as_value().as_str())
                .map(ToString::to_string)
        });
    let truncated = context_history_page
        .and_then(|page| page.get("hasMore"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            payload
                .as_ref()
                .and_then(|p| serde_json::to_string(p).ok())
                .is_some_and(|json| json.len() > 2048)
        });
    let history = HistoryContextV1 {
        hop,
        op: HistoryContextSessionOp::PageRead,
        status: HistoryContextStatus::Done,
        truncated,
        cursor,
        payload,
    };
    if let Ok(history_value) = serde_json::to_value(history) {
        output.insert("historyContext".to_string(), history_value);
    }
}

#[async_trait]
impl ToolSession for ProvenanceQuerySession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let send: ProvenanceQuerySendInput = serde_json::from_value(input)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e))))?;
        if let Some(read) = send.read.as_ref() {
            if let Some(mode) = read.mode.as_ref()
                && *mode != SessionReadMode::RetrieveRef
            {
                return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                    "unsupported read.mode for provenance session tool",
                )));
            }
            let projection = ProvenanceReadProjection::parse(read.projection.as_deref())?;
            let archive_ref = read.ref_id.as_str();
            let already_exhausted = self.retrieval_calls_used >= RETRIEVAL_CALLS_CAP
                || self.retrieval_bytes_used >= RETRIEVAL_BYTES_CAP
                || self.retrieval_items_used >= RETRIEVAL_ITEMS_CAP;
            if already_exhausted {
                self.pending = Some(
                    serde_json::to_value(self.budget_exhausted_output()).map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
                    })?,
                );
                return Ok(());
            }
            let record = self
                .query
                .resolve_archive_ref(archive_ref)
                .await
                .map_err(|e| {
                    ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::InvalidArgument(
                        e.to_string(),
                    )))
                })?;
            let items_in_record = record.as_ref().map_or(0u32, |rec| {
                u32::try_from(rec.payloads.len()).unwrap_or(u32::MAX)
            });
            let record_bytes = record.as_ref().map_or(0u32, |rec| {
                serde_json::to_vec(rec)
                    .map_or(0u32, |buf| u32::try_from(buf.len()).unwrap_or(u32::MAX))
            });
            let projected_calls = self.retrieval_calls_used.saturating_add(1);
            let projected_items = self.retrieval_items_used.saturating_add(items_in_record);
            let projected_bytes = self.retrieval_bytes_used.saturating_add(record_bytes);
            if projected_calls > RETRIEVAL_CALLS_CAP
                || projected_items > RETRIEVAL_ITEMS_CAP
                || projected_bytes > RETRIEVAL_BYTES_CAP
            {
                self.pending = Some(
                    serde_json::to_value(self.budget_exhausted_output()).map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
                    })?,
                );
                return Ok(());
            }
            self.retrieval_calls_used = projected_calls;
            self.retrieval_items_used = projected_items;
            self.retrieval_bytes_used = projected_bytes;
            let archive_record = record.map(archive_record_to_dto);
            let refs = collect_refs_with_prefix(archive_ref, archive_record.as_ref());
            let wrapped = ProvenanceQueryNextOutput {
                payload_json: None,
                read_result: Some(SessionReadResultDto {
                    mode: SessionReadMode::RetrieveRef,
                    ref_id: archive_ref.to_string(),
                    projection: projection.to_dto(),
                    refs,
                    archive_summary: match projection {
                        ProvenanceReadProjection::Identity => None,
                        ProvenanceReadProjection::Summary | ProvenanceReadProjection::Detail => {
                            archive_record.as_ref().map(summarize_record)
                        }
                    },
                    archive_record: match projection {
                        ProvenanceReadProjection::Detail => archive_record,
                        ProvenanceReadProjection::Identity | ProvenanceReadProjection::Summary => {
                            None
                        }
                    },
                }),
                context_history: None,
                retrieval_budget: Some(self.retrieval_budget()),
                budget_exhausted: Some(false),
                done: true,
                history_context: None,
            };
            self.pending = Some(serde_json::to_value(wrapped).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);
            return Ok(());
        }
        let resource = match parse_resource(&send.resource) {
            ProvenanceQueryResource::ContextHistory => {
                return self.send_context_history(&send).await;
            }
            ProvenanceQueryResource::Ops(resource) => resource,
        };
        let context_id = if self.scoped_to_context {
            Some(self.ctx.context_id.clone())
        } else {
            Some(
                send.context_id
                    .unwrap_or_else(|| self.ctx.context_id.clone()),
            )
        };
        let task_id = if self.scoped_to_context {
            self.ctx.task_id.clone()
        } else {
            send.task_id.or_else(|| self.ctx.task_id.clone())
        };
        let agent_id = if self.scoped_to_context {
            Some(self.ctx.agent_id.clone())
        } else {
            Some(send.agent_id.unwrap_or_else(|| self.ctx.agent_id.clone()))
        };
        let request = ProvenanceOpsQueryRequest {
            resource,
            filters: ProvenanceOpsFilters {
                context_id,
                task_id,
                agent_id,
                agent_package: None,
                provider: send.provider,
                model: send.model,
                tool_name: send.tool_name,
                tool_digest: None,
                baml_prompt: send.baml_prompt,
                payload_text: send.payload_text,
                from_timestamp_ms: None,
                to_timestamp_ms: None,
            },
            group_by: send.group_by.unwrap_or_default(),
            sort_by: send.sort_by,
            sort_dir: send.sort_dir,
            page_size: send.page_size,
            cursor: send.cursor,
            top_k: send.top_k,
            outcome: Some(parse_outcome(send.outcome.as_deref())),
            response_profile: Some(ProvenanceResponseProfile::ToolCompact),
            budget_mode: true,
            paginate_rows_in_sql: false,
        };
        let response = self.query.query_ops(request).await.map_err(|e| {
            ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::InvalidArgument(
                e.to_string(),
            )))
        })?;
        let output = ProvenanceQueryNextOutput {
            payload_json: Some(serde_json::to_string(&response).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?),
            read_result: None,
            context_history: None,
            retrieval_budget: Some(self.retrieval_budget()),
            budget_exhausted: Some(false),
            done: true,
            history_context: None,
        };
        self.pending =
            Some(serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        self.read_hop = self.read_hop.saturating_add(1);
        let mut payload = self.pending.take().unwrap_or(Value::Null);
        attach_history_context(&mut payload, self.read_hop);
        Ok(ToolStep::Done {
            output: Some(payload),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.pending = None;
        Ok(())
    }
}

struct ProvenanceQueryTool {
    metadata: ToolFunctionMetadata,
    scoped_to_context: bool,
    query: Arc<dyn ProvenanceOpsQuery>,
}

#[async_trait]
impl ToolHandler for ProvenanceQueryTool {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn describe_result_value(&self, _output: &Value) -> Option<String> {
        Some("provenance query results".to_string())
    }

    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return "provenance query: call".to_string(),
        };
        match op {
            "Open" => "querying provenance records".to_string(),
            "Send" => {
                if let Some(input) = step.get("input").and_then(|v| {
                    serde_json::from_value::<ProvenanceQuerySendInput>(v.clone()).ok()
                }) {
                    format!("querying provenance {}", input.resource)
                } else {
                    "querying provenance".to_string()
                }
            }
            "SearchRead" | "PageRead" => "reading provenance query output".to_string(),
            "Finish" => "finished provenance query".to_string(),
            "Abort" => "aborted provenance query".to_string(),
            other => format!("provenance query: {other}"),
        }
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        let _: ProvenanceQueryOpenInput =
            serde_json::from_value(open_input).map_err(BamlRtError::Json)?;
        Ok(Box::new(ProvenanceQuerySession {
            ctx,
            scoped_to_context: self.scoped_to_context,
            query: self.query.clone(),
            pending: None,
            retrieval_calls_used: 0,
            retrieval_bytes_used: 0,
            retrieval_items_used: 0,
            read_hop: 0,
        }))
    }
}

fn make_handler(
    metadata: ToolFunctionMetadata,
    scoped_to_context: bool,
    query: Arc<dyn ProvenanceOpsQuery>,
) -> Arc<dyn ToolHandler> {
    Arc::new(ProvenanceQueryTool {
        metadata,
        scoped_to_context,
        query,
    })
}

pub fn introspection_handler(query: Arc<dyn ProvenanceOpsQuery>) -> Arc<dyn ToolHandler> {
    make_handler(system_introspection_metadata(), true, query)
}

pub fn extrospection_handler(query: Arc<dyn ProvenanceOpsQuery>) -> Arc<dyn ToolHandler> {
    make_handler(system_extrospection_metadata(), false, query)
}
