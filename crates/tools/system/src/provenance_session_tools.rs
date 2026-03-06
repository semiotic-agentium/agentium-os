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

use crate::{
    metadata::{system_extrospection_metadata, system_introspection_metadata},
    tools::{ProvenanceQueryNextOutput, ProvenanceQueryOpenInput, ProvenanceQuerySendInput},
};

fn parse_resource(raw: &str) -> ProvenanceOpsResource {
    match raw.to_ascii_lowercase().as_str() {
        "tool_calls" => ProvenanceOpsResource::ToolCalls,
        "messages" => ProvenanceOpsResource::Messages,
        "aggregates" => ProvenanceOpsResource::Aggregates,
        _ => ProvenanceOpsResource::LlmCalls,
    }
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
}

#[async_trait]
impl ToolSession for ProvenanceQuerySession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let send: ProvenanceQuerySendInput = serde_json::from_value(input)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e))))?;
        let context_id = if self.scoped_to_context {
            Some(self.ctx.context_id.clone())
        } else {
            send.context_id
        };
        let agent_id = if self.scoped_to_context {
            Some(self.ctx.agent_id.clone())
        } else {
            send.agent_id
        };
        let request = ProvenanceOpsQueryRequest {
            resource: parse_resource(&send.resource),
            filters: ProvenanceOpsFilters {
                context_id,
                task_id: send.task_id,
                agent_id,
                provider: send.provider,
                model: send.model,
                tool_name: send.tool_name,
                baml_prompt: send.baml_prompt,
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
        };
        let response = self.query.query_ops(request).await.map_err(|e| {
            ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::InvalidArgument(
                e.to_string(),
            )))
        })?;
        let output = ProvenanceQueryNextOutput {
            payload_json: serde_json::to_string(&response).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?,
            done: true,
        };
        self.pending =
            Some(serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
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

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
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
