//! Context memory resolve tool implementation.
//!
//! Provides shared generic context retrieval API over provenance for agents
//! to query LLM calls, tool calls, and messages within their conversation context.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_provenance::{
    ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    ProvenanceOutcomeSegment, ProvenanceResponseProfile,
};
use baml_rt_tools::{
    ToolCapability, ToolFailure, ToolHandler, ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use serde_json::Value;

use crate::types::{
    ContextMemoryOutcome, ContextMemoryResolveNextOutput, ContextMemoryResolveOpenInput,
    ContextMemoryResolveRow, ContextMemoryResolveSendInput, ContextMemoryResource,
};

const DEFAULT_TOP_K: u32 = 50;

/// Get a string value from a JSON object, trying multiple key variants (e.g., snake_case and camelCase).
fn get_str<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
}

/// Get a u64 value from a JSON object, trying multiple key variants.
fn get_u64(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_u64))
}

/// Convert our tool-facing resource enum to provenance resource enum.
fn to_provenance_resource(resource: ContextMemoryResource) -> ProvenanceOpsResource {
    match resource {
        ContextMemoryResource::LlmCalls => ProvenanceOpsResource::LlmCalls,
        ContextMemoryResource::ToolCalls => ProvenanceOpsResource::ToolCalls,
        ContextMemoryResource::Messages => ProvenanceOpsResource::Messages,
    }
}

/// Convert our tool-facing outcome enum to provenance outcome enum.
fn to_provenance_outcome(outcome: Option<ContextMemoryOutcome>) -> ProvenanceOutcomeSegment {
    match outcome.unwrap_or_default() {
        ContextMemoryOutcome::FailedOnly => ProvenanceOutcomeSegment::FailedOnly,
        ContextMemoryOutcome::SuccessfulOnly => ProvenanceOutcomeSegment::SuccessfulOnly,
        ContextMemoryOutcome::Both => ProvenanceOutcomeSegment::Both,
    }
}

/// Extract a compact row from provenance response row.
fn extract_row(row: &Value, resource: ContextMemoryResource) -> Option<ContextMemoryResolveRow> {
    let obj = row.as_object()?;

    // Required fields - return None if missing to avoid polluting results with malformed rows
    let activity_id = get_str(obj, &["activity_id", "activityId"])?.to_string();
    let timestamp_ms = get_u64(obj, &["timestamp_ms", "timestampMs"])?;

    // Source type is determined by the resource we queried. Rows represent completed
    // activities with results, so we use llm_result/tool_result (not llm_call/tool_call).
    let source = match resource {
        ContextMemoryResource::LlmCalls => "llm_result",
        ContextMemoryResource::ToolCalls => "tool_result",
        ContextMemoryResource::Messages => "message",
    }
    .to_string();

    let agent_id = get_str(obj, &["agent_id", "agentId"]).map(ToString::to_string);
    let tool_name = get_str(obj, &["tool_name", "toolName"]).map(ToString::to_string);
    let outcome = get_str(obj, &["activity_outcome", "activityOutcome"]).map(ToString::to_string);

    // Build payload with relevant fields (snake_case canonical from provenance)
    let mut payload = serde_json::Map::new();
    let keep_fields = [
        // Common metadata
        "provider",
        "model",
        "baml_prompt",
        "duration_ms",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "error_class",
        "error_summary",
        // LLM call/result data
        "llm_call",
        "llm_result",
        // Tool call/result data
        "tool_call",
        "tool_result",
        // Message data
        "content",
        "role",
    ];

    for field in keep_fields {
        if let Some(value) = obj.get(field) {
            payload.insert(field.to_string(), value.clone());
        }
    }

    Some(ContextMemoryResolveRow {
        activity_id,
        timestamp_ms,
        source,
        agent_id,
        tool_name,
        outcome,
        payload: Value::Object(payload),
    })
}

/// Session state for context memory resolve.
struct ContextMemoryResolveSession {
    ctx: ToolSessionContext,
    query: Arc<dyn ProvenanceOpsQuery>,
    pending: Option<Value>,
}

#[async_trait]
impl ToolSession for ContextMemoryResolveSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let send: ContextMemoryResolveSendInput = serde_json::from_value(input)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e))))?;

        // Build provenance query request scoped to current context.
        // Note: task_id is intentionally None to query the entire context across all turns,
        // not just the current task. This enables cross-turn retrieval (e.g., team_id
        // discovered in turn 1 can be retrieved in turn 3).
        let request = ProvenanceOpsQueryRequest {
            resource: to_provenance_resource(send.resource),
            filters: ProvenanceOpsFilters {
                context_id: Some(self.ctx.context_id.clone()),
                task_id: None,
                agent_id: send.agent_id,
                provider: None,
                model: None,
                tool_name: send.tool_name,
                baml_prompt: None,
                payload_text: send.payload_text,
                from_timestamp_ms: send.from_timestamp_ms,
                to_timestamp_ms: send.to_timestamp_ms,
            },
            group_by: Vec::new(),
            sort_by: Some("timestamp_ms".to_string()),
            sort_dir: Some("desc".to_string()),
            page_size: send.top_k.or(Some(DEFAULT_TOP_K)),
            cursor: send.cursor,
            top_k: send.top_k.or(Some(DEFAULT_TOP_K)),
            outcome: Some(to_provenance_outcome(send.outcome)),
            response_profile: Some(ProvenanceResponseProfile::UiFull),
            budget_mode: true,
        };

        let response = self.query.query_ops(request).await.map_err(|e| {
            ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::ToolExecution(
                format!("provenance query failed: {e}"),
            )))
        })?;

        // Extract rows from response
        let rows: Vec<ContextMemoryResolveRow> = response
            .rows
            .iter()
            .filter_map(|row| {
                extract_row(row, send.resource).or_else(|| {
                    // Log at debug level with minimal info to avoid noisy/large payloads
                    let activity_id = row
                        .get("activity_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    tracing::debug!(
                        activity_id = %activity_id,
                        "Failed to extract row from provenance response, skipping"
                    );
                    None
                })
            })
            .collect();

        let returned_count = rows.len();
        let truncated = response.truncated;
        let next_cursor = response.next_cursor;

        let output = ContextMemoryResolveNextOutput {
            rows,
            returned_count,
            truncated,
            next_cursor,
            done: true,
        };

        self.pending =
            Some(serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);

        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let payload = self.pending.take().unwrap_or(Value::Null);
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

/// Tool handler for context memory resolve.
pub struct ContextMemoryResolveTool {
    metadata: ToolFunctionMetadata,
    query: Arc<dyn ProvenanceOpsQuery>,
}

impl ContextMemoryResolveTool {
    pub fn new(metadata: ToolFunctionMetadata, query: Arc<dyn ProvenanceOpsQuery>) -> Self {
        Self { metadata, query }
    }
}

#[async_trait]
impl ToolHandler for ContextMemoryResolveTool {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        let _: ContextMemoryResolveOpenInput =
            serde_json::from_value(open_input).map_err(BamlRtError::Json)?;

        Ok(Box::new(ContextMemoryResolveSession {
            ctx,
            query: self.query.clone(),
            pending: None,
        }))
    }
}

/// Create the context memory resolve handler.
pub fn context_memory_resolve_handler(
    metadata: ToolFunctionMetadata,
    query: Arc<dyn ProvenanceOpsQuery>,
) -> Arc<dyn ToolHandler> {
    Arc::new(ContextMemoryResolveTool::new(metadata, query))
}
