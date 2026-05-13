//! `ToolHandler` and `ToolSession` implementations for MCP-imported tools.
//!
//! One `McpToolHandler` per imported tool. Multiple handlers from the same
//! server share a single [`McpConnection`], so the runtime spawns one MCP
//! child per (server-id, server-config) regardless of how many tools point
//! at it.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolFailure, ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::runtime::{ConnectionError, McpConnection};

pub struct McpToolHandler {
    metadata: ToolFunctionMetadata,
    connection: Arc<McpConnection>,
    mcp_tool_name: String,
}

impl McpToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        connection: Arc<McpConnection>,
        mcp_tool_name: String,
    ) -> Self {
        Self {
            metadata,
            connection,
            mcp_tool_name,
        }
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(
        &self,
        _ctx: ToolSessionContext,
        _open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        Ok(Box::new(McpToolSession {
            connection: self.connection.clone(),
            mcp_tool_name: self.mcp_tool_name.clone(),
            state: Mutex::new(SessionState::Idle),
        }))
    }
}

#[derive(Debug, Default)]
enum SessionState {
    #[default]
    Idle,
    /// `send` has produced a result that `read` is expected to consume.
    Ready(Value),
    Aborted,
    Closed,
}

struct McpToolSession {
    connection: Arc<McpConnection>,
    mcp_tool_name: String,
    state: Mutex<SessionState>,
}

#[async_trait]
impl ToolSession for McpToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        {
            let state = self.state.lock().await;
            match &*state {
                SessionState::Aborted => {
                    return Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                        "MCP session already aborted",
                    )));
                }
                SessionState::Closed => {
                    return Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                        "MCP session already closed",
                    )));
                }
                SessionState::Ready(_) => {
                    return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                        "MCP session already has an unread result; call read before sending again",
                    )));
                }
                SessionState::Idle => {}
            }
            // Hold the lock only while validating state, drop before awaiting MCP.
            drop(state);
        }
        let result = self
            .connection
            .call_tool(&self.mcp_tool_name, input)
            .await
            .map_err(connection_error_to_session)?;
        let envelope = result_to_envelope(result);
        let mut state = self.state.lock().await;
        *state = SessionState::Ready(envelope);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let mut state = self.state.lock().await;
        match std::mem::replace(&mut *state, SessionState::Closed) {
            SessionState::Ready(output) => Ok(ToolStep::Done {
                output: Some(output),
            }),
            SessionState::Aborted => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session aborted",
            ))),
            SessionState::Closed => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session already closed",
            ))),
            SessionState::Idle => Err(ToolSessionError::Tool(ToolFailure::execution_failed(
                "MCP session has no pending result to read",
            ))),
        }
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        let mut state = self.state.lock().await;
        *state = SessionState::Closed;
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        let mut state = self.state.lock().await;
        *state = SessionState::Aborted;
        // Per-call MCP `notifications/cancelled` requires the in-flight request
        // id, which rmcp does not currently expose for `call_tool`. PR 5 will
        // either lift this up via a lower-level peer call or wait for an
        // upstream API. For now an aborted session refuses further reads and
        // the next tool call on the same connection is unaffected.
        Ok(())
    }
}

fn connection_error_to_session(err: ConnectionError) -> ToolSessionError {
    match err {
        ConnectionError::InvalidArguments(_) => {
            ToolSessionError::Tool(ToolFailure::invalid_input(err.to_string()))
        }
        ConnectionError::CallTool(_) | ConnectionError::InitializeFailed(_) => {
            ToolSessionError::Tool(ToolFailure::execution_failed(err.to_string()))
        }
        ConnectionError::InitializeTimeout(_) => {
            ToolSessionError::Tool(ToolFailure::execution_failed(err.to_string()))
        }
        ConnectionError::Spawn { .. } | ConnectionError::Transport(_) => {
            ToolSessionError::Transport(BamlRtError::InvalidArgument(err.to_string()))
        }
    }
}

/// Convert rmcp's `CallToolResult` into the platform's stable JSON envelope.
/// The shape mirrors the `ContentEnvelope` output mode locked in PR 1.
fn result_to_envelope(result: CallToolResult) -> Value {
    let mut content = Vec::with_capacity(result.content.len());
    for block in &result.content {
        match serde_json::to_value(block.raw.clone()) {
            Ok(value) => content.push(value),
            Err(err) => {
                tracing::warn!(error = %err, "failed to serialize MCP content block");
            }
        }
    }
    let structured = result
        .structured_content
        .map(|sc| serde_json::to_value(sc).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    json!({
        "content": content,
        "structured": structured,
        "is_error": result.is_error.unwrap_or(false),
        "metadata": Value::Null,
    })
}
