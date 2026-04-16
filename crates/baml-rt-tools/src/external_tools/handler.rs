//! FSM adapter: bridges the internal `ToolHandler`/`ToolSession` contract to the
//! stateless external invoke protocol.
//!
//! Semantics (per design doc §4.2):
//! - `open_session` → create lightweight adapter session with no upstream call.
//! - `send(input)` → **store** pending input (does NOT execute).
//! - `read()` right after `send()` → **perform** one `tool/invoke` and return `Done`.
//! - Repeated `send`+`read` cycles (`SessionPolicy::MultiSend`) trigger one invoke per cycle.
//! - `finish`/`abort` → best-effort cleanup (no-op; no durable state).

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::Result;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ToolName,
    tool_fsm::{ToolSession, ToolSessionError, ToolStep},
    tools::{ToolCapability, ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

use super::invoker::{ExternalInvoker, InvokeRequest};

/// `ToolHandler` that routes invocations through an [`ExternalInvoker`].
pub struct ProcessToolHandler {
    metadata: ToolFunctionMetadata,
    invoker: Arc<dyn ExternalInvoker>,
    /// Secrets resolved by the runner at registration time. Passed per-invocation.
    /// V1 resolves once at load; later phases may resolve per-call.
    secrets: serde_json::Map<String, Value>,
    /// Effective capabilities (policy intersection). Passed through to the tool.
    capabilities: Value,
    /// Per-invocation timeout.
    invoke_timeout: std::time::Duration,
}

impl ProcessToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        invoker: Arc<dyn ExternalInvoker>,
        invoke_timeout: std::time::Duration,
    ) -> Self {
        Self {
            metadata,
            invoker,
            secrets: serde_json::Map::new(),
            capabilities: Value::Null,
            invoke_timeout,
        }
    }

    pub fn with_secrets(mut self, secrets: serde_json::Map<String, Value>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = capabilities;
        self
    }
}

#[async_trait]
impl ToolHandler for ProcessToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::OneShot
    }

    async fn open_session(
        &self,
        _ctx: ToolSessionContext,
        _open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        Ok(Box::new(ProcessToolSession {
            tool_name: self.metadata.name.clone(),
            invoker: self.invoker.clone(),
            secrets: self.secrets.clone(),
            capabilities: self.capabilities.clone(),
            invoke_timeout: self.invoke_timeout,
            pending_input: None,
        }))
    }
}

/// Task-scoped adapter session. Holds no durable state; each `send`+`read`
/// cycle performs exactly one `tool/invoke`.
pub struct ProcessToolSession {
    tool_name: ToolName,
    invoker: Arc<dyn ExternalInvoker>,
    secrets: serde_json::Map<String, Value>,
    capabilities: Value,
    invoke_timeout: std::time::Duration,
    /// Input buffered by `send`, consumed by the next `read`.
    pending_input: Option<Value>,
}

#[async_trait]
impl ToolSession for ProcessToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        self.pending_input = Some(input);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let input = match self.pending_input.take() {
            Some(v) => v,
            None => {
                // No pending input — terminal no-op per FSM semantics.
                return Ok(ToolStep::Done { output: None });
            }
        };

        let request = InvokeRequest {
            tool_name: self.tool_name.clone(),
            invocation_id: Uuid::new_v4().to_string(),
            input,
            secrets: self.secrets.clone(),
            capabilities: self.capabilities.clone(),
            timeout: self.invoke_timeout,
        };

        let response = self.invoker.invoke(request).await?;
        Ok(ToolStep::Done {
            output: Some(response.output),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.pending_input = None;
        Ok(())
    }
}
