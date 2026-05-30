// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tests for QuickJS sandboxing.
//!
//! Single test runs all sandbox checks (require blocked, console.log works, fetch blocked)
//! with one agent build.

#![recursion_limit = "256"]

use std::sync::Arc;

use baml_rt::A2aAgent;
use serde_json::Value;

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
        (() => ({ available: typeof require !== 'undefined' }))()
    "#;
    let result = bridge.eval_sync(require_code).await;
    assert!(result.is_ok(), "require check should execute");
    let value = result.unwrap();
    assert_eq!(
        value.get("available").and_then(Value::as_bool),
        Some(false),
        "require must not be exposed to agent JS"
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
    let result = bridge.eval_sync(console_code).await;
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
        (() => ({ available: typeof fetch !== 'undefined' }))()
    "#;
    let result = bridge.eval_sync(fetch_code).await;
    assert!(result.is_ok(), "fetch check should execute");
    let value = result.unwrap();
    assert_eq!(
        value.get("available").and_then(Value::as_bool),
        Some(false),
        "fetch must not be exposed to agent JS"
    );
}
