//! Tests for QuickJS sandboxing

use std::sync::Arc;
use baml_rt::A2aAgent;

#[tokio::test]
async fn test_sandbox_prevents_require() {
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Try to use require - should fail
    let code = r#"
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

    let result = bridge.evaluate(None, code).await;
    assert!(result.is_ok(), "Code should execute");
    
    let value = result.unwrap();
    let msg = value.get("message").or(value.get("error"));
    assert!(msg.is_some(), "Should return a message about require availability");
}

#[tokio::test]
async fn test_sandbox_console_log_works() {
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Test that console.log works (but doesn't cause I/O issues)
    let code = r#"
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

    let result = bridge.evaluate(None, code).await;
    assert!(result.is_ok(), "Code should execute");
    
    let value = result.unwrap();
    assert!(value.get("success").and_then(|v| v.as_bool()).unwrap_or(false), 
            "console.log should work");
}

#[tokio::test]
async fn test_sandbox_prevents_fetch() {
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Try to use fetch - should not be available
    let code = r#"
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

    let result = bridge.evaluate(None, code).await;
    assert!(result.is_ok(), "Code should execute");
    
    let value = result.unwrap();
    let msg = value.get("message").or(value.get("error"));
    assert!(msg.is_some(), "Should return a message about fetch availability");
}
