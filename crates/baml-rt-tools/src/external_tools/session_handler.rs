// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `ExternalSessionToolHandler`.
//!
//! Maps the host [`ToolHandler`] / [`ToolSession`] contract onto a
//! [`SessionToolInvoker`]. Per `plans/sandbox_streaming.md` §5.2 this handler
//! is the external-side analogue of the internal session FSM: each host
//! `ToolSession::{send, read, finish, abort}` call routes to one
//! `tool/session_*` RPC.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;
use uuid::Uuid;

use super::{
    metadata::ExternalSecretScope,
    session_invoker::{
        SessionAbortRequest, SessionFinishRequest, SessionOpenRequest, SessionReadRequest,
        SessionSendRequest, SessionToolInvoker,
    },
};
use crate::{
    ToolName,
    tool_fsm::{ToolSession, ToolSessionError, ToolStep},
    tools::{ToolCapability, ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

/// Default chunk-read timeout used until metadata-driven values are wired in.
pub(crate) const DEFAULT_CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// Handler for sandbox session-mode tools.
///
/// Compile-only Phase 1 skeleton: holds the metadata + invoker, advertises
/// [`ToolCapability::Streaming`], and routes `open_session` to the invoker.
/// The session inner-loop bookkeeping (resume tokens, single reader, reset
/// path) lands in Phase 3.
type SessionInvokerFactory =
    Arc<dyn Fn(&ToolSessionContext) -> Arc<dyn SessionToolInvoker> + Send + Sync>;

enum SessionInvokerBinding {
    Static(Arc<dyn SessionToolInvoker>),
    Factory(SessionInvokerFactory),
}

pub struct ExternalSessionToolHandler {
    metadata: ToolFunctionMetadata,
    invoker: SessionInvokerBinding,
    open_timeout: Duration,
    send_timeout: Duration,
    chunk_timeout: Duration,
    finish_timeout: Duration,
    abort_timeout: Duration,
    secrets: serde_json::Map<String, Value>,
    capabilities: Value,
    secret_scope: ExternalSecretScope,
}

impl ExternalSessionToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        invoker: Arc<dyn SessionToolInvoker>,
        open_timeout: Duration,
    ) -> Self {
        Self {
            metadata,
            invoker: SessionInvokerBinding::Static(invoker),
            open_timeout,
            send_timeout: open_timeout,
            chunk_timeout: DEFAULT_CHUNK_TIMEOUT,
            finish_timeout: open_timeout,
            abort_timeout: Duration::from_secs(2),
            secrets: serde_json::Map::new(),
            capabilities: Value::Null,
            secret_scope: ExternalSecretScope::Send,
        }
    }

    pub fn new_with_factory(
        metadata: ToolFunctionMetadata,
        invoker_factory: SessionInvokerFactory,
        open_timeout: Duration,
    ) -> Self {
        Self {
            metadata,
            invoker: SessionInvokerBinding::Factory(invoker_factory),
            open_timeout,
            send_timeout: open_timeout,
            chunk_timeout: DEFAULT_CHUNK_TIMEOUT,
            finish_timeout: open_timeout,
            abort_timeout: Duration::from_secs(2),
            secrets: serde_json::Map::new(),
            capabilities: Value::Null,
            secret_scope: ExternalSecretScope::Send,
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

    pub fn with_chunk_timeout(mut self, chunk_timeout: Duration) -> Self {
        self.chunk_timeout = chunk_timeout;
        self
    }

    pub fn with_abort_timeout(mut self, abort_timeout: Duration) -> Self {
        self.abort_timeout = abort_timeout;
        self
    }

    pub fn with_secret_scope(mut self, secret_scope: ExternalSecretScope) -> Self {
        self.secret_scope = secret_scope;
        self
    }
}

#[async_trait]
impl ToolHandler for ExternalSessionToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        _ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        let invoker: Arc<dyn SessionToolInvoker> = match &self.invoker {
            SessionInvokerBinding::Static(invoker) => invoker.clone(),
            SessionInvokerBinding::Factory(factory) => factory(&_ctx),
        };

        let req = SessionOpenRequest {
            tool_name: self.metadata.name.clone(),
            invocation_id: Uuid::new_v4().to_string(),
            open_input,
            secrets: match self.secret_scope {
                ExternalSecretScope::Send => serde_json::Map::new(),
                ExternalSecretScope::Session => self.secrets.clone(),
            },
            capabilities: self.capabilities.clone(),
            timeout: self.open_timeout,
        };

        let response = invoker.session_open(req).await?;

        Ok(Box::new(ExternalSessionToolSession {
            tool_name: self.metadata.name.clone(),
            invoker,
            session_id: response.session_id,
            pending_resume_token: response.initial_resume_token,
            initial_step: response.initial_step,
            send_timeout: self.send_timeout,
            chunk_timeout: self.chunk_timeout,
            finish_timeout: self.finish_timeout,
            abort_timeout: self.abort_timeout,
            secrets: self.secrets.clone(),
            secret_scope: self.secret_scope,
        }))
    }
}

/// Per-task session adapter. Maps each [`ToolSession`] call onto one
/// `tool/session_*` RPC against the [`SessionToolInvoker`].
///
pub struct ExternalSessionToolSession {
    #[expect(
        dead_code,
        reason = "reserved for span attributes / classified errors in a later phase"
    )]
    tool_name: ToolName,
    invoker: Arc<dyn SessionToolInvoker>,
    session_id: String,
    /// Set when the last observed step was `Suspended`; cleared on the next
    /// `send`. Phase 3 will validate this against caller-supplied tokens.
    pending_resume_token: Option<String>,
    /// First step delivered alongside `session_open`. Surfaced to the caller
    /// on the very next `read` before any `session_read` RPC fires.
    initial_step: Option<ToolStep>,
    send_timeout: Duration,
    chunk_timeout: Duration,
    finish_timeout: Duration,
    abort_timeout: Duration,
    secrets: serde_json::Map<String, Value>,
    secret_scope: ExternalSecretScope,
}

#[async_trait]
impl ToolSession for ExternalSessionToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let resume_token = self.pending_resume_token.take();
        let req = SessionSendRequest {
            session_id: self.session_id.clone(),
            input,
            resume_token,
            secrets: match self.secret_scope {
                ExternalSecretScope::Send => self.secrets.clone(),
                ExternalSecretScope::Session => serde_json::Map::new(),
            },
            timeout: self.send_timeout,
        };
        self.invoker.session_send(req).await?;
        Ok(())
    }

    async fn read(&mut self, input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        if !is_empty_payload(&input) {
            return Err(BamlRtError::InvalidArgument(
                "external session tools require empty `read()` payloads; use `send(input)` then `read(())`"
                    .to_string(),
            )
            .into());
        }

        if let Some(step) = self.initial_step.take() {
            return Ok(step);
        }

        let req = SessionReadRequest {
            session_id: self.session_id.clone(),
            chunk_timeout: self.chunk_timeout,
        };
        let response = self.invoker.session_read(req).await?;
        self.pending_resume_token = response.resume_token;
        Ok(response.step)
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        let req = SessionFinishRequest {
            session_id: self.session_id.clone(),
            timeout: self.finish_timeout,
        };
        self.invoker.session_finish(req).await?;
        Ok(())
    }

    async fn abort(&mut self, reason: Option<String>) -> std::result::Result<(), ToolSessionError> {
        let req = SessionAbortRequest {
            session_id: self.session_id.clone(),
            reason,
            timeout: self.abort_timeout,
        };
        self.invoker.session_abort(req).await?;
        Ok(())
    }
}

fn is_empty_payload(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}
