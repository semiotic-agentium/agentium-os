// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
use baml_rt_core::{BamlRtError, ClassifiedToolError, ErrorDisposition, Result, clock_events};
use serde_json::Value;
use uuid::Uuid;

use super::{
    ExternalLifecycleEvent, ExternalLifecycleRecorder,
    invoker::{ExternalInvoker, InvokeRequest},
    policy::{InvocationPolicy, PolicyError, QuarantineState, ToolQuota},
};
use crate::{
    ToolName,
    tool_fsm::{ToolSession, ToolSessionError, ToolStep},
    tools::{ToolCapability, ToolFunctionMetadata, ToolHandler, ToolSessionContext},
};

/// `ToolHandler` that routes invocations through an [`ExternalInvoker`].
pub struct ProcessToolHandler {
    metadata: ToolFunctionMetadata,
    invoker: Arc<dyn ExternalInvoker>,
    pub(crate) policy: Arc<InvocationPolicy>,
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    /// Secrets resolved by the runner at registration time. Passed per-invocation.
    /// V1 resolves once at load; later phases may resolve per-call.
    secrets: serde_json::Map<String, Value>,
    /// Effective capabilities (policy intersection). Passed through to the tool.
    capabilities: Value,
}

impl ProcessToolHandler {
    pub fn new(
        metadata: ToolFunctionMetadata,
        invoker: Arc<dyn ExternalInvoker>,
        invoke_timeout: std::time::Duration,
    ) -> Self {
        let quota = ToolQuota {
            invoke_timeout,
            ..ToolQuota::default()
        };

        Self {
            policy: Arc::new(InvocationPolicy::new(metadata.name.clone(), quota)),
            metadata,
            invoker,
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

    pub fn with_policy_quota(mut self, quota: ToolQuota) -> Self {
        self.policy = Arc::new(InvocationPolicy::new(self.metadata.name.clone(), quota));
        self
    }

    pub fn with_lifecycle_recorder(mut self, recorder: ExternalLifecycleRecorder) -> Self {
        self.lifecycle_recorder = Some(recorder);
        self
    }

    pub async fn lift_quarantine(&self, lifted_by: &str) {
        self.policy.lift_quarantine().await;
        if let Some(recorder) = &self.lifecycle_recorder {
            recorder(ExternalLifecycleEvent::QuarantineLifted {
                tool_name: self.metadata.name.to_string(),
                lifted_by: lifted_by.to_string(),
                lifted_at_ms: baml_rt_core::now_unix_ms(clock_events::EXTERNAL_QUARANTINE_LIFT),
            });
        }
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
            policy: self.policy.clone(),
            lifecycle_recorder: self.lifecycle_recorder.clone(),
            secrets: self.secrets.clone(),
            capabilities: self.capabilities.clone(),
            pending_input: None,
        }))
    }
}

/// Task-scoped adapter session. Holds no durable state; each `send`+`read`
/// cycle performs exactly one `tool/invoke`.
pub struct ProcessToolSession {
    tool_name: ToolName,
    invoker: Arc<dyn ExternalInvoker>,
    policy: Arc<InvocationPolicy>,
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    secrets: serde_json::Map<String, Value>,
    capabilities: Value,
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
                        started_at_ms: baml_rt_core::now_unix_ms(clock_events::EXTERNAL_QUARANTINE),
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
            code: format!("external_{}_quarantined", tool),
            disposition: ErrorDisposition::Fatal,
            message: format!("tool '{tool}' is quarantined: {reason}"),
            hint: Some("Tool is temporarily disabled after repeated failures; lift quarantine to resume invocations".to_string()),
            retry_after_ms: None,
        }),
        PolicyError::ConcurrencyExhausted { .. } => {
            BamlRtError::ToolClassified(ClassifiedToolError {
                code: format!("external_{}_concurrency_exhausted", tool),
                disposition: ErrorDisposition::HostRetriable,
                message: format!("tool '{tool}' concurrency slot unavailable"),
                hint: Some("Retry after another in-flight invocation completes".to_string()),
                retry_after_ms: Some(100),
            })
        }
    }
}

fn with_backoff_retry_after(err: BamlRtError, backoff: std::time::Duration) -> BamlRtError {
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use baml_rt_core::{
        Result,
        ids::{AgentId, ContextId, UuidId},
    };
    use serde_json::json;
    use tokio::{sync::Semaphore, time::Duration};

    use super::*;
    use crate::{
        external_tools::{
            invoker::{InvokeResponse, ToolDescribe},
            protocol::{METHOD_INVOKE, ToolSchemaResult},
        },
        tool_fsm::ToolSessionId,
        tools::{ToolAccess, ToolBackend, ToolOrigin, ToolSessionContext, ToolTypeSpec},
    };

    struct BlockingMockInvoker {
        gate: Arc<Semaphore>,
        starts: AtomicUsize,
        inflight: AtomicUsize,
        max_inflight: AtomicUsize,
    }

    impl BlockingMockInvoker {
        fn new(gate: Arc<Semaphore>) -> Self {
            Self {
                gate,
                starts: AtomicUsize::new(0),
                inflight: AtomicUsize::new(0),
                max_inflight: AtomicUsize::new(0),
            }
        }

        fn starts(&self) -> usize {
            self.starts.load(Ordering::SeqCst)
        }

        fn max_inflight(&self) -> usize {
            self.max_inflight.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ExternalInvoker for BlockingMockInvoker {
        async fn describe(&self, tool: &ToolName, _timeout: Duration) -> Result<ToolDescribe> {
            Ok(ToolDescribe {
                protocol_version: "1".to_string(),
                tool_name: tool.to_string(),
                supported_methods: vec![METHOD_INVOKE.to_string()],
                max_payload_bytes: None,
                schema_digest: None,
                capabilities: None,
            })
        }

        async fn schema(&self, tool: &ToolName, _timeout: Duration) -> Result<ToolSchemaResult> {
            Err(BamlRtError::InvalidArgument(format!(
                "tool/schema not supported by mock invoker for {tool}"
            )))
        }

        async fn invoke(&self, _req: InvokeRequest) -> Result<InvokeResponse> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            let mut current_max = self.max_inflight.load(Ordering::SeqCst);
            while now > current_max {
                match self.max_inflight.compare_exchange(
                    current_max,
                    now,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(observed) => current_max = observed,
                }
            }

            let _permit = self
                .gate
                .clone()
                .acquire_owned()
                .await
                .expect("gate closed");

            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(InvokeResponse {
                output: json!({"ok": true}),
                done: true,
            })
        }
    }

    struct FlakyMockInvoker {
        failures_before_success: usize,
        calls: AtomicUsize,
    }

    impl FlakyMockInvoker {
        fn new(failures_before_success: usize) -> Self {
            Self {
                failures_before_success,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ExternalInvoker for FlakyMockInvoker {
        async fn describe(&self, tool: &ToolName, _timeout: Duration) -> Result<ToolDescribe> {
            Ok(ToolDescribe {
                protocol_version: "1".to_string(),
                tool_name: tool.to_string(),
                supported_methods: vec![METHOD_INVOKE.to_string()],
                max_payload_bytes: None,
                schema_digest: None,
                capabilities: None,
            })
        }

        async fn schema(&self, tool: &ToolName, _timeout: Duration) -> Result<ToolSchemaResult> {
            Err(BamlRtError::InvalidArgument(format!(
                "tool/schema not supported by mock invoker for {tool}"
            )))
        }

        async fn invoke(&self, _req: InvokeRequest) -> Result<InvokeResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call <= self.failures_before_success {
                return Err(BamlRtError::ToolExecution("boom".to_string()));
            }

            Ok(InvokeResponse {
                output: json!({"ok": true}),
                done: true,
            })
        }
    }

    #[tokio::test]
    async fn enforces_default_concurrency_with_policy() {
        let tool_name = ToolName::parse("support/concurrency").unwrap();
        let metadata = sample_metadata(tool_name.clone());
        let gate = Arc::new(Semaphore::new(0));
        let invoker = Arc::new(BlockingMockInvoker::new(gate.clone()));

        let handler = ProcessToolHandler::new(metadata, invoker.clone(), Duration::from_secs(30))
            .with_policy_quota(ToolQuota {
                max_concurrent: 1,
                ..ToolQuota::default()
            });

        let mut session_a = handler
            .open_session(sample_session_ctx("support/concurrency"), Value::Null)
            .await
            .expect("open a");
        let mut session_b = handler
            .open_session(sample_session_ctx("support/concurrency"), Value::Null)
            .await
            .expect("open b");

        session_a.send(json!({"n": 1})).await.expect("send a");
        session_b.send(json!({"n": 2})).await.expect("send b");

        let read_a = tokio::spawn(async move { session_a.read(Value::Null).await });
        wait_until(|| invoker.starts() == 1).await;

        let read_b = tokio::spawn(async move { session_b.read(Value::Null).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            invoker.starts(),
            1,
            "second invocation should wait on policy semaphore"
        );

        gate.add_permits(1);
        let _ = read_a.await.unwrap().expect("read a ok");

        wait_until(|| invoker.starts() == 2).await;
        gate.add_permits(1);
        let _ = read_b.await.unwrap().expect("read b ok");

        assert_eq!(
            invoker.max_inflight(),
            1,
            "policy must cap concurrency to 1"
        );
    }

    #[tokio::test]
    async fn quarantine_trips_and_lift_restores_invocation() {
        let tool_name = ToolName::parse("support/quarantine").unwrap();
        let metadata = sample_metadata(tool_name);
        let invoker = Arc::new(FlakyMockInvoker::new(2));

        let quota = ToolQuota {
            quarantine_threshold: 2,
            ..ToolQuota::default()
        };

        let handler = ProcessToolHandler::new(metadata, invoker.clone(), Duration::from_secs(30))
            .with_policy_quota(quota);

        let mut session = handler
            .open_session(sample_session_ctx("support/quarantine"), Value::Null)
            .await
            .expect("open");

        session.send(json!({"attempt": 1})).await.unwrap();
        let err_1 = session
            .read(Value::Null)
            .await
            .expect_err("first call fails");
        assert_retry_after(err_1, Some(1_000));

        session.send(json!({"attempt": 2})).await.unwrap();
        let err_2 = session
            .read(Value::Null)
            .await
            .expect_err("second call fails");
        assert_retry_after(err_2, Some(2_000));

        session.send(json!({"attempt": 3})).await.unwrap();
        let quarantined = session
            .read(Value::Null)
            .await
            .expect_err("third call blocked by quarantine");

        match quarantined {
            ToolSessionError::Transport(BamlRtError::ToolClassified(classified)) => {
                assert!(classified.code.contains("quarantined"));
                assert_eq!(classified.disposition, ErrorDisposition::Fatal);
            }
            other => panic!("expected quarantined ToolClassified error, got {other:?}"),
        }

        assert_eq!(
            invoker.calls(),
            2,
            "quarantined call must not reach invoker"
        );

        handler.policy.lift_quarantine().await;

        session.send(json!({"attempt": 4})).await.unwrap();
        let step = session
            .read(Value::Null)
            .await
            .expect("lifted call succeeds");
        assert!(matches!(step, ToolStep::Done { .. }));
        assert_eq!(invoker.calls(), 3);
    }

    #[tokio::test]
    async fn emits_quarantine_and_lifted_lifecycle_events() {
        let tool_name = ToolName::parse("support/quarantine_events").unwrap();
        let metadata = sample_metadata(tool_name);
        let invoker = Arc::new(FlakyMockInvoker::new(1));

        let events = Arc::new(std::sync::Mutex::new(Vec::<ExternalLifecycleEvent>::new()));
        let recorder: ExternalLifecycleRecorder = {
            let events = events.clone();
            Arc::new(move |event| {
                events.lock().unwrap().push(event);
            })
        };

        let quota = ToolQuota {
            quarantine_threshold: 1,
            ..ToolQuota::default()
        };

        let handler = ProcessToolHandler::new(metadata, invoker, Duration::from_secs(30))
            .with_policy_quota(quota)
            .with_lifecycle_recorder(recorder);

        let mut session = handler
            .open_session(sample_session_ctx("support/quarantine_events"), Value::Null)
            .await
            .expect("open");

        session.send(json!({"attempt": 1})).await.unwrap();
        let _ = session.read(Value::Null).await.expect_err("must fail");

        handler.lift_quarantine("test").await;

        let captured = events.lock().unwrap();
        assert!(captured.iter().any(|e| matches!(
            e,
            ExternalLifecycleEvent::Quarantine {
                tool_name,
                consecutive_failures: 1,
                ..
            } if tool_name == "support/quarantine_events"
        )));
        assert!(captured.iter().any(|e| matches!(
            e,
            ExternalLifecycleEvent::QuarantineLifted { tool_name, .. }
            if tool_name == "support/quarantine_events"
        )));
    }

    #[tokio::test]
    async fn repeated_send_read_cycles_in_same_session_invoke_each_time() {
        let tool_name = ToolName::parse("support/multisend_adapter").unwrap();
        let metadata = sample_metadata(tool_name);
        let invoker = Arc::new(FlakyMockInvoker::new(0));

        let handler = ProcessToolHandler::new(metadata, invoker.clone(), Duration::from_secs(30));

        let mut session = handler
            .open_session(sample_session_ctx("support/multisend_adapter"), Value::Null)
            .await
            .expect("open");

        session.send(json!({"attempt": 1})).await.unwrap();
        let step_1 = session
            .read(Value::Null)
            .await
            .expect("first read succeeds");
        assert!(matches!(step_1, ToolStep::Done { .. }));

        session.send(json!({"attempt": 2})).await.unwrap();
        let step_2 = session
            .read(Value::Null)
            .await
            .expect("second read succeeds");
        assert!(matches!(step_2, ToolStep::Done { .. }));

        assert_eq!(
            invoker.calls(),
            2,
            "each send+read cycle in one session should perform one invoke"
        );
    }

    fn assert_retry_after(err: ToolSessionError, expected_ms: Option<u64>) {
        match err {
            ToolSessionError::Transport(BamlRtError::ToolClassified(classified)) => {
                assert_eq!(classified.retry_after_ms, expected_ms);
            }
            other => panic!("expected ToolClassified error, got {other:?}"),
        }
    }

    async fn wait_until(check: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met before timeout");
    }

    fn sample_session_ctx(tool_name: &str) -> ToolSessionContext {
        ToolSessionContext {
            session_id: ToolSessionId::random(),
            tool_name: ToolName::parse(tool_name).unwrap(),
            context_id: ContextId::new(1, 1),
            agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
            config: None,
            config_version: None,
            task_id: None,
            execution_classifier: None,
        }
    }

    fn sample_metadata(name: ToolName) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name,
            class_name: "SupportSample".to_string(),
            description: "sample".to_string(),
            open_input_schema: json!({}),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            open_input_type: ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: "SupportSampleInput".to_string(),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: "SupportSampleOutput".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: Some(ToolAccess::Read),
            tags: Vec::new(),
            secret_requests: Vec::new(),
            config: None,
            config_bundle: None,
            origin: ToolOrigin::Host,
            backend: ToolBackend::External,
            digest: None,
            projection_semantics: None,
            session_policy: Default::default(),
            event_sources: Vec::new(),
            coordination_baml: None,
        }
    }
}
