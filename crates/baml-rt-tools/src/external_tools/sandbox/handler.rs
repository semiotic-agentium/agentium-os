//! [`ToolHandler`] wrapping the sandbox backend (`tool_sandbox.md` §11 Phase C
//! step 4 + step 5). Mirrors the semantics of
//! [`super::super::handler::ProcessToolHandler`] but constructs a fresh
//! [`SandboxInvoker`] per session so each session picks up the right
//! `(agent_id, context_id)` scope for the §9.2 cache key.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, ClassifiedToolError, ErrorDisposition, Result, clock_events, ids::AgentId,
};
use serde_json::Value;
use uuid::Uuid;

use super::{
    invoker::{SandboxCache, SandboxInvoker, SandboxSpecBuilder},
    provider::SandboxProvider,
};
use crate::{
    ToolName,
    external_tools::{
        ExternalLifecycleEvent, ExternalLifecycleRecorder,
        invoker::{InvokeRequest, ToolInvoker},
        policy::{InvocationPolicy, PolicyError, QuarantineState, ToolQuota},
    },
    tool_fsm::{ToolSession, ToolSessionError, ToolStep},
    tools::{ToolCapability, ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

/// Sandbox-backed `ToolHandler`. One per tool; shared across contexts.
///
/// The provider + cache are shared; the per-session invoker is scoped to the
/// session's `(agent_id, context_id)`.
pub struct SandboxToolHandler {
    metadata: ToolFunctionMetadata,
    provider: Arc<dyn SandboxProvider>,
    cache: Arc<SandboxCache>,
    build_spec: SandboxSpecBuilder,
    policy: Arc<InvocationPolicy>,
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    secrets: serde_json::Map<String, Value>,
    capabilities: Value,
}

impl SandboxToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        provider: Arc<dyn SandboxProvider>,
        cache: Arc<SandboxCache>,
        build_spec: SandboxSpecBuilder,
        invoke_timeout: Duration,
    ) -> Self {
        let quota = ToolQuota {
            invoke_timeout,
            ..ToolQuota::default()
        };
        Self {
            policy: Arc::new(InvocationPolicy::new(metadata.name.clone(), quota)),
            metadata,
            provider,
            cache,
            build_spec,
            lifecycle_recorder: None,
            secrets: serde_json::Map::new(),
            capabilities: Value::Null,
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

    pub fn with_lifecycle_recorder(mut self, recorder: ExternalLifecycleRecorder) -> Self {
        self.lifecycle_recorder = Some(recorder);
        self
    }
}

#[async_trait]
impl ToolHandler for SandboxToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::OneShot
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        _open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        let invoker: Arc<dyn ToolInvoker> = Arc::new(SandboxInvoker::new(
            self.provider.clone(),
            self.cache.clone(),
            self.build_spec.clone(),
            ctx.agent_id.clone(),
            ctx.context_id.clone(),
        ));
        Ok(Box::new(SandboxToolSession {
            tool_name: self.metadata.name.clone(),
            invoker,
            policy: self.policy.clone(),
            lifecycle_recorder: self.lifecycle_recorder.clone(),
            secrets: self.secrets.clone(),
            capabilities: self.capabilities.clone(),
            pending_input: None,
            _agent_id: ctx.agent_id,
        }))
    }
}

/// Per-session adapter. Same FSM shape as the process backend; invoker is
/// scope-bound so the cache key resolves correctly on each `read`.
pub struct SandboxToolSession {
    tool_name: ToolName,
    invoker: Arc<dyn ToolInvoker>,
    policy: Arc<InvocationPolicy>,
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    secrets: serde_json::Map<String, Value>,
    capabilities: Value,
    pending_input: Option<Value>,
    _agent_id: AgentId,
}

#[async_trait]
impl ToolSession for SandboxToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        self.pending_input = Some(input);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let input = match self.pending_input.take() {
            Some(v) => v,
            None => return Ok(ToolStep::Done { output: None }),
        };

        let _permit = self
            .policy
            .acquire()
            .await
            .map_err(|err| map_policy_error(&self.tool_name, err))?;

        let request = InvokeRequest {
            tool_name: self.tool_name.clone(),
            invocation_id: Uuid::new_v4().to_string(),
            input,
            secrets: self.secrets.clone(),
            capabilities: self.capabilities.clone(),
            timeout: self.policy.quota().invoke_timeout,
        };

        match self.invoker.invoke(request).await {
            Ok(response) => {
                self.policy.record_success().await;
                Ok(ToolStep::Done {
                    output: Some(response.output),
                })
            }
            Err(err) => {
                let was_quarantined = matches!(
                    self.policy.current_state().await,
                    QuarantineState::Quarantined { .. }
                );
                let backoff = self.policy.record_failure(err.to_string()).await;
                if !was_quarantined
                    && let QuarantineState::Quarantined {
                        consecutive_failures,
                        reason,
                        ..
                    } = self.policy.current_state().await
                    && let Some(recorder) = &self.lifecycle_recorder
                {
                    recorder(ExternalLifecycleEvent::Quarantine {
                        tool_name: self.tool_name.to_string(),
                        reason,
                        consecutive_failures,
                        started_at_ms: baml_rt_core::now_unix_ms(clock_events::SANDBOX_QUARANTINE),
                    });
                }
                Err(with_backoff_retry_after(err, backoff).into())
            }
        }
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

fn map_policy_error(tool: &ToolName, err: PolicyError) -> BamlRtError {
    match err {
        PolicyError::Quarantined { reason, .. } => BamlRtError::ToolClassified(ClassifiedToolError {
            code: format!("sandbox_{tool}_quarantined"),
            disposition: ErrorDisposition::Fatal,
            message: format!("sandbox tool '{tool}' is quarantined: {reason}"),
            hint: Some(
                "Tool is temporarily disabled after repeated failures; lift quarantine to resume"
                    .to_string(),
            ),
            retry_after_ms: None,
        }),
        PolicyError::ConcurrencyExhausted { .. } => BamlRtError::ToolClassified(ClassifiedToolError {
            code: format!("sandbox_{tool}_concurrency_exhausted"),
            disposition: ErrorDisposition::HostRetriable,
            message: format!("sandbox tool '{tool}' concurrency slot unavailable"),
            hint: Some("Retry after another in-flight invocation completes".to_string()),
            retry_after_ms: Some(100),
        }),
    }
}

fn with_backoff_retry_after(err: BamlRtError, backoff: Duration) -> BamlRtError {
    let backoff_ms = Some(backoff.as_millis().min(u128::from(u64::MAX)) as u64);
    match err {
        BamlRtError::ToolClassified(mut classified) => {
            if classified.retry_after_ms.is_none() {
                classified.retry_after_ms = backoff_ms;
            }
            BamlRtError::ToolClassified(classified)
        }
        other => {
            let mut classified = ClassifiedToolError::from_baml_error(&other);
            if classified.retry_after_ms.is_none() {
                classified.retry_after_ms = backoff_ms;
            }
            BamlRtError::ToolClassified(classified)
        }
    }
}
