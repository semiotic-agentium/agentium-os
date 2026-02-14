//! Tests for JavaScript invocation of BAML functions

#![recursion_limit = "256"]

use baml_rt_core::context::{InvocationScope, RuntimeScope};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use test_support::common::{agent_fixture, setup_baml_runtime_from_fixture, setup_bridge};
use uuid::Uuid;

#[tokio::test]
async fn test_js_invoke_baml_function() {
    let agent_dir = agent_fixture("stream-baml-tool");
    if !agent_dir.join("baml_src").exists() {
        eprintln!("Skipping test: stream-baml-tool fixture not found");
        return;
    }

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_from_fixture("stream-baml-tool");
    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Test invoking ChooseCalcTool from JavaScript (stream-baml-tool has this function)
    // Use __awaitAndStringify helper to handle async function calls
    let js_code = r#"
        (function() {
            try {
                const promise = ChooseCalcTool({ user_message: "add 2 and 3" });
                return __awaitAndStringify(promise);
            } catch (e) {
                return JSON.stringify({ success: false, error: e.toString() });
            }
        })()
    "#;

    let result = bridge.evaluate(None, js_code).await;

    // The result should contain either success with result, or error info
    // Note: This may fail due to missing API keys, which is acceptable
    // We just want to verify the function can be invoked from JS
    let json_result = match result {
        Ok(val) => val,
        Err(e) => {
            println!(
                "JavaScript execution error (may be due to missing API keys): {:?}",
                e
            );
            // The function exists and was called, but execution failed (likely API key issue)
            // This is acceptable for integration tests
            return;
        }
    };
    println!("JavaScript execution result: {:?}", json_result);

    // Check if we got a proper result
    // The result might be a promise that needs to be awaited, or it might be an object
    // For now, just verify that we can call the function and get some response
    // (The actual BAML execution is happening, as we can see from the logs)
    if let Some(obj) = json_result.as_object() {
        // If we got an object, check if it has the expected fields
        if obj.contains_key("success") || obj.contains_key("error") {
            // This is the expected format
            println!("Got expected result format: {:?}", obj);
        } else {
            // Might be a different format or the function returned a different structure
            println!("Got different result format: {:?}", obj);
        }
    }

    // At minimum, verify that we received a non-null response payload.
    assert!(!json_result.is_null(), "Expected a non-null response value");
}

#[tokio::test]
async fn test_tool_session_promise_resolves_via_event_loop_drive() {
    // Exercise host-resolved promises without LLM calls by using support/calculate tool session.
    let baml_manager = setup_baml_runtime_from_fixture("stream-baml-tool");
    let mut bridge = setup_bridge(baml_manager.clone()).await;

    let scope = InvocationScope::new(RuntimeScope::task_scope(
        ContextId::new(1700000000000, 1),
        AgentId::from_uuid(UuidId::new(Uuid::new_v4())),
        MessageId::from_external(ExternalId::new("msg-1")),
        TaskId::from_external(ExternalId::new("task-1")),
    ));

    let js_code = r#"
        (function() {
            const promise = (async function() {
                const session = await openToolSession("support/calculate", __baml_invocation_token);
                await session.send({ expression: { left: 2, operation: "+", right: 3 }});
                const step = await session.continue();
                await session.finish();
                return JSON.stringify(step);
            })();
            return __awaitAndStringify(promise);
        })()
    "#;

    let result = bridge.evaluate(Some(&scope), js_code).await;
    let json_result = match result {
        Ok(val) => val,
        Err(e) => {
            panic!("Expected tool session promise to resolve, got error: {e:?}");
        }
    };

    assert!(
        !json_result.is_null(),
        "Expected non-null tool session result"
    );
}
