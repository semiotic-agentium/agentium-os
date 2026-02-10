//! Property tests for `A2aRequestHandler::run_handle_a2a` queue/drain semantics.
//!
//! This suite deliberately uses malformed A2A requests so the handler returns JSON-RPC
//! error responses without relying on JS integration behavior.
//!
//! Invariant:
//!   ∀ submitted request r_i, exactly one response envelope is returned.
//! Liveness:
//!   ∀ submitted request r_i, completion occurs within bounded time.

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::context::RuntimeScope;
use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
use proptest::prelude::*;
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(6))]

    /// PROPERTY:
    /// ∀ N malformed requests submitted through run_handle_a2a:
    ///   - each future resolves within T
    ///   - each result contains exactly one JSON-RPC error response
    #[test]
    fn prop_run_handle_a2a_malformed_requests_are_bounded_and_single_response(n in 1u32..=12u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        rt.block_on(async move {
            let agent = A2aAgent::builder()
                .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
                .build()
                .await
                .expect("agent build");

            let mut joins = Vec::with_capacity(n as usize);
            for i in 0..n {
                let agent = agent.clone();
                joins.push(tokio::spawn(async move {
                    let context_id = ContextId::new(777, i as u64 + 1);
                    let message_id =
                        MessageId::from_external(ExternalId::new(format!("prop-msg-{}", i)));
                    let scope = RuntimeScope::message_scope(
                        context_id,
                        agent.agent_id().clone(),
                        message_id,
                    );
                    let malformed_request = json!({ "foo": format!("bad-{i}") });
                    timeout(
                        Duration::from_secs(2),
                        agent.run_handle_a2a(scope, malformed_request),
                    )
                    .await
                }));
            }

            for join in joins {
                let timed = join.await.expect("task join");
                let outcome = timed.expect("run_handle_a2a timeout");
                let responses = outcome.expect("run_handle_a2a result");
                assert_eq!(responses.len(), 1, "exactly one response envelope");
                let response = &responses[0];
                assert!(
                    response.get("error").is_some(),
                    "malformed request must produce JSON-RPC error envelope: {response}"
                );
            }
        });
    }
}
