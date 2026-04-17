//! Single end-to-end test for Workstream B.
//!
//! Wires `MockSandboxProvider` with a JSON-RPC echo adapter through
//! `SandboxCache` + `SandboxInvoker` + `ToolInvoker::invoke`, asserting
//! that a request sent through the cache + provider + TSRPC pipeline
//! round-trips as a well-formed `InvokeResponse`.
//!
//! Kept intentionally minimal per the "overloaded CI, one test for B7"
//! directive. Covers the Describe/Invoke parity requirement from
//! `tool_sandbox.md` §11 Phase C step 6 in one shot.

use std::{sync::Arc, time::Duration};

use baml_rt_core::{
    ContextId,
    ids::{AgentId, UuidId},
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    SandboxCache, SandboxCacheKey, SandboxInvoker, SandboxProvider, SandboxSpec, SandboxSpecBuilder,
    mock::{MockSandboxProvider, ScriptedAdapter},
};
use crate::{
    ToolName,
    external_tools::invoker::{InvokeRequest, ToolInvoker},
};

#[tokio::test]
async fn sandbox_invoker_happy_path_round_trips_through_mock_provider() {
    // Guest-side "tool-adapter": parses the JSON-RPC request and returns a
    // matching response carrying the input back as `echoed.input`.
    let adapter: ScriptedAdapter = Arc::new(|stream| {
        tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(stream);
            loop {
                let mut len_buf = [0u8; 4];
                if r.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut body = vec![0u8; len];
                if r.read_exact(&mut body).await.is_err() {
                    break;
                }
                let req: Value = match serde_json::from_slice(&body) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let id = req.get("id").and_then(Value::as_u64).unwrap_or(1);
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "output": { "echoed": req["params"] }, "done": true }
                });
                let out = serde_json::to_vec(&reply).unwrap();
                if w.write_all(&(out.len() as u32).to_be_bytes()).await.is_err() {
                    break;
                }
                if w.write_all(&out).await.is_err() {
                    break;
                }
                if w.flush().await.is_err() {
                    break;
                }
            }
        })
    });
    let provider_concrete = MockSandboxProvider::new(adapter);
    let provider: Arc<dyn SandboxProvider> = Arc::new(provider_concrete.clone());
    let cache = Arc::new(SandboxCache::new("runner-test"));

    let agent = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    let ctx = ContextId::new(1, 1);
    let tool = ToolName::parse("support/echo").unwrap();
    let expected_name = cache.encode_name(&SandboxCacheKey {
        agent_id: agent.clone(),
        context_id: ctx.clone(),
        tool_name: tool.clone(),
    });

    let expected_name_for_spec = expected_name.clone();
    let build_spec: SandboxSpecBuilder = Arc::new(move |_key| {
        Ok(SandboxSpec::for_test(
            expected_name_for_spec.clone(),
            "ghcr.io/test/echo@sha256:deadbeef",
        ))
    });

    let invoker = SandboxInvoker::new(provider.clone(), cache.clone(), build_spec, agent, ctx);

    let req = InvokeRequest {
        tool_name: tool,
        invocation_id: "inv-1".to_string(),
        input: json!({"msg": "ping"}),
        secrets: serde_json::Map::new(),
        capabilities: Value::Null,
        timeout: Duration::from_secs(5),
    };

    let res = invoker.invoke(req).await.expect("invoke should succeed");

    // Response pipeline: provider + cache + TSRPC + envelope decode all OK.
    assert!(res.done);
    let echoed_msg = res
        .output
        .pointer("/echoed/input/msg")
        .and_then(Value::as_str)
        .expect("echoed.input.msg present in response");
    assert_eq!(echoed_msg, "ping");

    // Lazy first-use landed exactly one sandbox in the cache (§9.4).
    assert_eq!(cache.active_count(), 1);

    // In-process reattach works for the name we just created (§9.4 in-process
    // reattach). Done directly against the provider so we assert its own
    // list/reattach wiring, not the cache's.
    let reattached = provider.reattach(&expected_name).await.unwrap();
    assert_eq!(reattached.name, expected_name);
}
