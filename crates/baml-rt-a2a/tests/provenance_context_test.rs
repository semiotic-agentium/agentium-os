#![cfg(feature = "falkordb-tests")]
#![recursion_limit = "256"]

mod common;

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::ContextId;
use baml_rt_provenance::{FalkorDbProvenanceConfig, FalkorDbProvenanceWriter};
use serde_json::Value;
use std::sync::Arc;
use test_support::common::send_stream_request;
use test_support::common::shared_falkordb;
use test_support::support::a2a::A2aInMemoryClient;
use tokio::time::Duration;

async fn setup_agent(writer: Arc<FalkorDbProvenanceWriter>) -> A2aAgent {
    let js_code = r#"
        globalThis.onChatMessage = async function(message) {
            __chat_yield({
                task: {
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_WORKING" }
                }
            });
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } }
            });
        };
    "#;
    common::provenance::build_provenance_agent(writer, js_code).await
}

fn expect_context_id(responses: Vec<Value>) -> String {
    for response in responses {
        let result = response.get("result").cloned().unwrap_or(response);
        let content = result.get("chunk").cloned().unwrap_or(result);
        if let Some(task) = content.get("task").and_then(Value::as_object)
            && let Some(context_id) = task.get("contextId").and_then(Value::as_str)
        {
            return context_id.to_string();
        }
    }
    panic!("task");
}

#[tokio::test]
async fn test_context_id_propagates_across_agents() {
    let connection = shared_falkordb().await;
    let graph1 = format!("baml_a2a_ctx_prop_{}_1", std::process::id());
    let graph2 = format!("baml_a2a_ctx_prop_{}_2", std::process::id());

    let writer1 = Arc::new(FalkorDbProvenanceWriter::new(
        FalkorDbProvenanceConfig::new(connection.to_owned(), graph1),
    ));
    let writer2 = Arc::new(FalkorDbProvenanceWriter::new(
        FalkorDbProvenanceConfig::new(connection.to_owned(), graph2),
    ));
    let agent1 = setup_agent(writer1).await;
    let agent2 = setup_agent(writer2.clone()).await;

    let request = send_stream_request("msg-1", "hello", "corr-2-1", None);
    let responses = agent1.handle_a2a(request).await.expect("a2a handle");
    let context_id = expect_context_id(responses);

    let client = A2aInMemoryClient::new(Arc::new(agent2));
    let request = send_stream_request(
        "msg-2",
        "forward",
        "corr-2-2",
        Some(ContextId::parse_temporal(&context_id).expect("context id")),
    );
    let responses = client.send(request).await.expect("agent2 handle");
    let propagated = expect_context_id(responses);
    assert_eq!(
        propagated, context_id,
        "expected forwarded request to preserve context id across agents"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_context_id_is_task_local_under_concurrency() {
    let connection = shared_falkordb().await;
    let graph = format!("baml_a2a_ctx_concurrency_{}", std::process::id());
    let writer = Arc::new(FalkorDbProvenanceWriter::new(
        FalkorDbProvenanceConfig::new(connection.to_owned(), graph),
    ));
    let agent = setup_agent(writer).await;

    let context_ids: Vec<ContextId> = (0..4).map(|i| ContextId::new(10, i as u64)).collect();
    for (idx, context_id) in context_ids.iter().enumerate() {
        let request = send_stream_request(
            &format!("msg-{idx}"),
            "hello",
            &format!("corr-2-{}", idx + 3),
            Some(context_id.clone()),
        );
        let responses = tokio::time::timeout(Duration::from_secs(15), agent.handle_a2a(request))
            .await
            .expect("request timeout")
            .expect("a2a handle");
        let got = expect_context_id(responses);
        assert_eq!(
            got,
            context_id.as_str().to_string(),
            "expected response context_id to match request context_id"
        );
    }
}
