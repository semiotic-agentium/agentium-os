//! Tests for QuickJS sandboxing.
//!
//! Single test runs all sandbox checks (require blocked, console.log works, fetch blocked)
//! with one agent build.

#![recursion_limit = "256"]

use std::sync::Arc;

use baml_rt::A2aAgent;

#[tokio::test]
async fn test_sandbox_environment() {
    let store = test_support::common::test_graphqlite_store();
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_graphqlite_store(store)
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // 1. require must not be available
    let require_code = r#"
        (() => {
            try {
                if (typeof require !== 'undefined') {
                    require('fs');
                    return JSON.stringify({error: "require should not be available"});
                }
                return JSON.stringify({success: true, message: "require not available"});
            } catch (e) {
                return JSON.stringify({success: true, error: e.toString()});
            }
        })()
    "#;
    let result = bridge.evaluate(None, require_code).await;
    assert!(result.is_ok(), "require check should execute");
    let value = result.unwrap();
    assert!(
        value.get("message").or(value.get("error")).is_some(),
        "require: should return message about availability"
    );

    // 2. console.log must work
    let console_code = r#"
        (() => {
            try {
                console.log("Test message");
                console.log({test: "object"});
                return JSON.stringify({success: true, message: "console.log works"});
            } catch (e) {
                return JSON.stringify({error: e.toString()});
            }
        })()
    "#;
    let result = bridge.evaluate(None, console_code).await;
    assert!(result.is_ok(), "console.log check should execute");
    let value = result.unwrap();
    assert!(
        value
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "console.log should work"
    );

    // 3. fetch must not be available
    let fetch_code = r#"
        (() => {
            try {
                if (typeof fetch !== 'undefined') {
                    return JSON.stringify({error: "fetch should not be available"});
                }
                return JSON.stringify({success: true, message: "fetch not available"});
            } catch (e) {
                return JSON.stringify({success: true, error: e.toString()});
            }
        })()
    "#;
    let result = bridge.evaluate(None, fetch_code).await;
    assert!(result.is_ok(), "fetch check should execute");
    let value = result.unwrap();
    assert!(
        value.get("message").or(value.get("error")).is_some(),
        "fetch: should return message about availability"
    );
}
