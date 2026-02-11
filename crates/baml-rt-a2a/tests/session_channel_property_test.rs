//! Property tests for channel-based A2A dispatcher/worker invariants and liveness.
//!
//! Core properties:
//! 1. Dispatcher forwards each registered `Send` exactly once with preserved scope.
//! 2. Finish removes session routing (no more forwards).
//! 3. Worker eventually emits a response and binds it to message-carried scope.

#![recursion_limit = "256"]

use async_trait::async_trait;
use baml_rt_a2a::A2aRequestHandler;
use baml_rt_a2a::session_channel::{
    DispatcherMsg, RuntimeWorkerMsg, SessionCmd, run_dispatcher, run_runtime_worker,
};
use baml_rt_core::context::RuntimeScope;
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, UuidId};
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ToolSessionId;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

fn test_scope(context_counter: u64) -> RuntimeScope {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000f1").unwrap());
    let context_id = ContextId::new(1234, context_counter);
    let message_id = MessageId::from_external(ExternalId::new(format!("msg-{}", context_counter)));
    RuntimeScope::message_scope(context_id, agent_id, message_id)
}

#[derive(Clone)]
struct MockHandler;

#[async_trait(?Send)]
impl A2aRequestHandler for MockHandler {
    async fn handle_a2a(&self, request: Value) -> Result<Vec<Value>> {
        let scope = baml_rt_core::context::current_scope()
            .map_err(|_| BamlRtError::ToolExecution("missing scope in mock handler".to_string()))?;
        Ok(vec![json!({
            "context_id": scope.context_id().to_string(),
            "echo": request,
        })])
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// PROPERTY 1 (dispatcher invariant):
    /// ∀ i ∈ [0, n): Register(session, scope) then Cmd(Send(req_i)) forwards exactly one
    /// HandleA2a_i with the same scope and request payload.
    #[test]
    fn prop_dispatcher_forwards_registered_scope_and_payload(n in 1usize..=12usize) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let (dispatcher_tx, dispatcher_rx) = mpsc::unbounded_channel::<DispatcherMsg>();
            let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<RuntimeWorkerMsg>();
            tokio::spawn(run_dispatcher(dispatcher_rx, worker_tx));

            let session_id = ToolSessionId::new(Uuid::new_v4());
            let scope = test_scope(1);
            let (response_tx, _response_rx) = mpsc::unbounded_channel::<Value>();
            dispatcher_tx.send(DispatcherMsg::Register {
                session_id: session_id.clone(),
                scope: scope.clone(),
                response_tx,
            }).expect("register send");

            for i in 0..n {
                dispatcher_tx.send(DispatcherMsg::Cmd {
                    session_id: session_id.clone(),
                    cmd: SessionCmd::Send(json!({"idx": i})),
                }).expect("cmd send");
            }

            for i in 0..n {
                let msg = timeout(Duration::from_secs(1), worker_rx.recv())
                    .await
                    .expect("worker recv timeout")
                    .expect("worker msg");
                match msg {
                    RuntimeWorkerMsg::HandleA2a { scope: got_scope, request, .. } => {
                        assert_eq!(got_scope.context_id(), scope.context_id());
                        assert_eq!(request, json!({"idx": i}));
                    }
                }
            }
        });
    }

    /// PROPERTY 2 (dispatcher invariant):
    /// After Cmd(Finish), subsequent Cmd(Send(_)) must not be forwarded.
    #[test]
    fn prop_dispatcher_finish_removes_session_and_blocks_further_sends(seed in 1u64..=100u64) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let (dispatcher_tx, dispatcher_rx) = mpsc::unbounded_channel::<DispatcherMsg>();
            let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<RuntimeWorkerMsg>();
            tokio::spawn(run_dispatcher(dispatcher_rx, worker_tx));

            let session_id = ToolSessionId::new(Uuid::new_v4());
            let scope = test_scope(seed);
            let (response_tx, _response_rx) = mpsc::unbounded_channel::<Value>();
            dispatcher_tx.send(DispatcherMsg::Register {
                session_id: session_id.clone(),
                scope,
                response_tx,
            }).expect("register send");
            dispatcher_tx.send(DispatcherMsg::Cmd {
                session_id: session_id.clone(),
                cmd: SessionCmd::Finish,
            }).expect("finish send");
            dispatcher_tx.send(DispatcherMsg::Cmd {
                session_id,
                cmd: SessionCmd::Send(json!({"after_finish": true})),
            }).expect("send after finish");

            let recv_attempt = timeout(Duration::from_millis(200), worker_rx.recv()).await;
            assert!(recv_attempt.is_err(), "no worker message expected after finish");
        });
    }

    /// PROPERTY 3 (worker liveness + scope invariant):
    /// IF HandleA2a(scope, request, tx) is enqueued, THEN eventually tx receives a response
    /// whose context attribution matches `scope.context_id`.
    #[test]
    fn prop_worker_eventual_response_with_message_scope(seed in 1u64..=64u64) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let (worker_tx, worker_rx) = mpsc::unbounded_channel::<RuntimeWorkerMsg>();
            let handler: Arc<dyn A2aRequestHandler> = Arc::new(MockHandler);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                rt.block_on(run_runtime_worker(handler, worker_rx));
            });

            let scope = test_scope(seed);
            let request = json!({"payload": seed});
            let (response_tx, mut response_rx) = mpsc::unbounded_channel::<Value>();
            worker_tx.send(RuntimeWorkerMsg::HandleA2a {
                scope: scope.clone(),
                request: request.clone(),
                response_tx,
            }).expect("worker send");

            let response = timeout(Duration::from_secs(1), response_rx.recv())
                .await
                .expect("response timeout")
                .expect("response value");
            assert_eq!(
                response.get("context_id").and_then(Value::as_str),
                Some(scope.context_id().to_string().as_str())
            );
            assert_eq!(response.get("echo"), Some(&request));
        });
    }
}
