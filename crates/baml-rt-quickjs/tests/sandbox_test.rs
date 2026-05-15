//! Tests for QuickJS sandboxing.
//!
//! Single test runs all sandbox checks (require blocked, console.log works, fetch blocked)
//! with one agent build.

#![recursion_limit = "256"]

use std::sync::Arc;

use baml_rt::A2aAgent;

#[tokio::test]
async fn test_sandbox_environment() {
    let store = test_support::common::test_surreal_store().await;
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_surreal_store(store)
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // 1. require must not be available
    let require_code = r#"
        (() => JSON.stringify({ available: typeof require !== 'undefined' }))()
    "#;
    let value = bridge
        .eval_sync(require_code)
        .await
        .expect("require check should execute");
    assert_eq!(
        value.get("available").and_then(serde_json::Value::as_bool),
        Some(false),
        "require must not be defined in the QuickJS sandbox"
    );

    // 2. console.log must work
    let console_code = r#"
        (() => {
            try {
                console.log("Test message");
                console.log({test: "object"});
                return JSON.stringify({success: true});
            } catch (e) {
                return JSON.stringify({success: false, error: e.toString()});
            }
        })()
    "#;
    let value = bridge
        .eval_sync(console_code)
        .await
        .expect("console.log check should execute");
    assert_eq!(
        value.get("success").and_then(serde_json::Value::as_bool),
        Some(true),
        "console.log must work (error: {:?})",
        value.get("error")
    );

    // 3. fetch must not be available
    let fetch_code = r#"
        (() => JSON.stringify({ available: typeof fetch !== 'undefined' }))()
    "#;
    let value = bridge
        .eval_sync(fetch_code)
        .await
        .expect("fetch check should execute");
    assert_eq!(
        value.get("available").and_then(serde_json::Value::as_bool),
        Some(false),
        "fetch must not be defined in the QuickJS sandbox"
    );
}
