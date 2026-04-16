//! Property tests for effect system invariants.
//!
//! These tests validate system-level invariants with mocked I/O and simulated hangs:
//! - Effect start/complete pairing
//! - Liveness gating behavior
//! - Provenance admissibility
//!
//! ## Invariants tested:
//!
//! **I1 (Start/Complete Pairing)**

#![recursion_limit = "256"]
//!   For every `EffectEvent::*Started`, there must be exactly one corresponding
//!   `EffectEvent::*Completed` with the same context_id and kind.
//!
//! **I2 (Liveness Gating)**
//!   When effects are in-flight, the poller uses max_attempts_ms (configurable, default 30 minutes).
//!   When no effects are in-flight, the poller uses idle_timeout_attempts (5s).
//!
//!   Tests use shorter timeouts (e.g., 1000ms) for faster feedback.
//!
//! **I3 (Effect Count Accuracy)**
//!   In-flight counts accurately reflect Started - Completed events.
//!
//! **I4 (Provenance Admissibility)**
//!   Provenance events require message_id in metadata (runtime validation).

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{
    Outcome,
    bus::{
        A2aEffectMetadata, A2aLivenessRole, BusWithEffects, EffectEmitter, EffectEvent,
        EffectLiveness, InFlightCounts, ToolEffectMetadata,
    },
    ids::{AgentId, ContextId, UuidId},
};
use proptest::prelude::*;
use serde_json::json;
use tokio::{
    sync::RwLock,
    time::{Duration, timeout},
};

fn proptest_cfg(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(cases);
    cfg.failure_persistence = None;
    cfg
}

/// Test timeout for max_attempts_ms - much shorter than production default (30 minutes)
/// to enable fast test feedback while still validating timeout behavior.
const TEST_MAX_ATTEMPTS_MS: u64 = 1000; // 1 second for tests

/// Watchdog timeout for liveness tests - ensures tests fail fast if they hang.
/// Should be longer than TEST_MAX_ATTEMPTS_MS but short enough for CI feedback.
const TEST_WATCHDOG_TIMEOUT_MS: u64 = 5000; // 5 seconds watchdog

/// Mock effect liveness that can simulate hangs.
struct MockEffectLiveness {
    counts: Arc<RwLock<HashMap<ContextId, InFlightCounts>>>,
    always_in_flight: bool, // Simulate hang: always report in-flight > 0
}

impl MockEffectLiveness {
    fn new(always_in_flight: bool) -> Self {
        Self {
            counts: Arc::new(RwLock::new(HashMap::new())),
            always_in_flight,
        }
    }

    async fn set_counts(&self, context_id: ContextId, counts: InFlightCounts) {
        let mut map = self.counts.write().await;
        if counts.any() {
            map.insert(context_id, counts);
        } else {
            map.remove(&context_id);
        }
    }
}

#[async_trait::async_trait]
impl EffectLiveness for MockEffectLiveness {
    async fn in_flight(&self, context_id: &ContextId) -> InFlightCounts {
        if self.always_in_flight {
            // Simulate hang: always report effects in-flight
            InFlightCounts {
                tool: 1,
                llm: 0,
                a2a: 0,
                a2a_command: 0,
            }
        } else {
            let map = self.counts.read().await;
            map.get(context_id).copied().unwrap_or_default()
        }
    }
}

/// Test invariant I2: Liveness gating with mocked liveness.
///
/// When effects are in-flight, poller should use long timeout.
/// When no effects, poller should use short timeout.
#[tokio::test]
async fn test_liveness_gating_timeout() {
    timeout(
        Duration::from_millis(TEST_WATCHDOG_TIMEOUT_MS),
        test_liveness_gating_timeout_inner(),
    )
    .await
    .expect("Test timed out - liveness test hung");
}

async fn test_liveness_gating_timeout_inner() {
    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;

    let context_id = ContextId::new(1000, 2);
    let bus = Arc::new(BusWithEffects::new());

    // Test 1: No effects in-flight -> short timeout
    let poller = EffectGatedTimeoutPolicy::new(
        bus.clone() as Arc<dyn EffectLiveness>,
        context_id.clone(),
        5000,                 // 5s idle timeout
        TEST_MAX_ATTEMPTS_MS, // Short timeout for tests
    );
    let timeout_attempts = poller.timeout_attempts().await;
    assert_eq!(
        timeout_attempts, 5000,
        "I2: No effects should use idle timeout"
    );

    // Test 2: Effects in-flight -> long timeout
    let metadata = ToolEffectMetadata {
        tool_name: "test_tool".to_string(),
        function_name: None,
        args: json!({}),
        metadata: json!({"message_id": "msg-2"}),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };
    bus.emit(EffectEvent::ToolStarted {
        context_id: context_id.clone(),
        metadata: metadata.clone(),
    })
    .await
    .unwrap();

    let timeout_attempts = poller.timeout_attempts().await;
    assert_eq!(
        timeout_attempts, TEST_MAX_ATTEMPTS_MS as u32,
        "I2: Effects in-flight should use max_attempts"
    );

    // Clean up
    bus.emit(EffectEvent::ToolCompleted {
        context_id,
        metadata,
        duration_ms: 100,
        outcome: Outcome::Success,
        result: None,
    })
    .await
    .unwrap();
}

/// Test simulated hang: Effect Started without Completed (token leak).
///
/// This simulates a scenario where an effect is started but never completed,
/// which should be detected by liveness gating.
#[tokio::test]
async fn test_simulated_hang_started_without_completed() {
    let bus = Arc::new(BusWithEffects::new());
    let context_id = ContextId::new(1000, 4);

    // Start effect but never complete (simulated leak)
    let metadata = ToolEffectMetadata {
        tool_name: "hanging_tool".to_string(),
        function_name: None,
        args: json!({}),
        metadata: json!({"message_id": "msg-hang"}),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };
    bus.emit(EffectEvent::ToolStarted {
        context_id: context_id.clone(),
        metadata,
    })
    .await
    .unwrap();

    // Effect should remain in-flight indefinitely
    let counts = bus.in_flight(&context_id).await;
    assert_eq!(counts.tool, 1, "Hang test: Effect should remain in-flight");
    assert!(counts.any(), "Hang test: Should detect in-flight effect");

    // Liveness gating should use long timeout
    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;
    let poller = EffectGatedTimeoutPolicy::new(
        bus.clone() as Arc<dyn EffectLiveness>,
        context_id,
        5000,
        TEST_MAX_ATTEMPTS_MS, // Short timeout for tests
    );
    let timeout_attempts = poller.timeout_attempts().await;
    assert_eq!(
        timeout_attempts, TEST_MAX_ATTEMPTS_MS as u32,
        "Hang test: Should use max_attempts when effect is hanging"
    );
}

/// Test simulated hang: EffectLiveness always reports in-flight > 0.
///
/// This simulates a buggy liveness tracker that always reports effects in-flight,
/// causing the poller to never timeout.
#[tokio::test]
async fn test_simulated_hang_always_in_flight() {
    timeout(
        Duration::from_millis(TEST_WATCHDOG_TIMEOUT_MS),
        test_simulated_hang_always_in_flight_inner(),
    )
    .await
    .expect("Test timed out - always-in-flight hang test hung");
}

async fn test_simulated_hang_always_in_flight_inner() {
    let mock_liveness = Arc::new(MockEffectLiveness::new(true));
    let context_id = ContextId::new(1000, 5);

    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;
    let poller = EffectGatedTimeoutPolicy::new(
        mock_liveness.clone() as Arc<dyn EffectLiveness>,
        context_id,
        5000,
        TEST_MAX_ATTEMPTS_MS, // Short timeout for tests
    );

    // Even with no actual effects, mock reports in-flight
    let timeout_attempts = poller.timeout_attempts().await;
    assert_eq!(
        timeout_attempts, TEST_MAX_ATTEMPTS_MS as u32,
        "Hang test: Mock always-in-flight should use max_attempts"
    );
}

/// set_counts sets in-flight state without bus events; poller should use long timeout.
#[tokio::test]
async fn test_set_counts_directly_sets_in_flight() {
    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;

    let mock = Arc::new(MockEffectLiveness::new(false));
    let context_id = ContextId::new(1000, 10);
    mock.set_counts(
        context_id.clone(),
        InFlightCounts {
            tool: 1,
            llm: 0,
            a2a: 0,
            a2a_command: 0,
        },
    )
    .await;

    let poller = EffectGatedTimeoutPolicy::new(
        mock as Arc<dyn EffectLiveness>,
        context_id,
        5000,
        TEST_MAX_ATTEMPTS_MS,
    );
    let timeout_attempts = poller.timeout_attempts().await;
    assert_eq!(
        timeout_attempts, TEST_MAX_ATTEMPTS_MS as u32,
        "set_counts in-flight should yield max_attempts timeout"
    );
}

/// CG6: Timeout monotonicity - poller returns values that may decrease when effects complete;
/// the evaluate() loop uses max(initial, new) so timeout never decreases mid-loop.
#[tokio::test]
async fn test_timeout_monotonicity_effect_completion() {
    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;

    let bus = Arc::new(BusWithEffects::new());
    let context_id = ContextId::new(1000, 98);
    let idle = 5000u32;
    let max_attempts = TEST_MAX_ATTEMPTS_MS as u32;

    let poller = EffectGatedTimeoutPolicy::new(
        bus.clone() as Arc<dyn EffectLiveness>,
        context_id.clone(),
        idle as u64,
        max_attempts as u64,
    );

    let t_no_effect = poller.timeout_attempts().await;
    assert_eq!(t_no_effect, idle, "No effects: idle timeout");

    let metadata = ToolEffectMetadata {
        tool_name: "mono_tool".to_string(),
        function_name: None,
        args: json!({}),
        metadata: json!({"message_id": "msg-mono"}),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };
    bus.emit(EffectEvent::ToolStarted {
        context_id: context_id.clone(),
        metadata: metadata.clone(),
    })
    .await
    .unwrap();

    let t_with_effect = poller.timeout_attempts().await;
    assert_eq!(t_with_effect, max_attempts, "With effect: max timeout");

    bus.emit(EffectEvent::ToolCompleted {
        context_id,
        metadata,
        duration_ms: 1,
        outcome: Outcome::Success,
        result: None,
    })
    .await
    .unwrap();

    let t_after_complete = poller.timeout_attempts().await;
    assert_eq!(t_after_complete, idle, "After complete: idle again");

    // CG6: evaluate() loop uses max(initial_timeout, new_timeout) so the effective
    // timeout never decreases mid-loop. Here we only assert state-dependent values;
    // monotonicity is enforced in quickjs_bridge evaluate().
}

/// A2A envelope effect alone should not force long timeout.
/// Long timeout is reserved for downstream progress-capable work.
#[tokio::test]
async fn test_a2a_envelope_is_discounted_for_polling() {
    use baml_rt_quickjs::quickjs_bridge::EffectGatedTimeoutPolicy;

    let bus = Arc::new(BusWithEffects::new());
    let context_id = ContextId::new(1000, 97);
    let idle = 5000u32;
    let max_attempts = TEST_MAX_ATTEMPTS_MS as u32;
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000097").unwrap());
    let metadata = A2aEffectMetadata {
        agent_id,
        method: "message.sendStream".to_string(),
        request_id: Some("req-97".to_string()),
        liveness_role: A2aLivenessRole::Command,
        metadata: json!({ "phase": "envelope" }),
    };
    let effect_metadata = A2aEffectMetadata {
        agent_id: metadata.agent_id.clone(),
        method: metadata.method.clone(),
        request_id: metadata.request_id.clone(),
        liveness_role: A2aLivenessRole::Effect,
        metadata: json!({ "phase": "nested" }),
    };

    let policy = EffectGatedTimeoutPolicy::new(
        bus.clone() as Arc<dyn EffectLiveness>,
        context_id.clone(),
        idle as u64,
        max_attempts as u64,
    );

    // Envelope in flight => still idle timeout.
    bus.emit(EffectEvent::A2aStarted {
        context_id: context_id.clone(),
        metadata: metadata.clone(),
    })
    .await
    .unwrap();
    let one_a2a = policy.timeout_attempts().await;
    assert_eq!(
        one_a2a, idle,
        "Single A2A envelope should not force long timeout"
    );

    // Nested/child A2A effect => long timeout.
    bus.emit(EffectEvent::A2aStarted {
        context_id: context_id.clone(),
        metadata: effect_metadata,
    })
    .await
    .unwrap();
    let nested_a2a = policy.timeout_attempts().await;
    assert_eq!(
        nested_a2a, max_attempts,
        "Nested A2A work should force long timeout"
    );
}

/// E1 / CG3 (release only): Dropping EffectStartToken without complete leaves in-flight count at 1.
///
/// In debug builds, Drop panics; in release we only log. This test runs in release to assert
/// the leak is observable (in_flight stays 1).
#[tokio::test]
#[cfg(not(debug_assertions))]
async fn test_effect_token_drop_leaves_in_flight() {
    let bus = Arc::new(BusWithEffects::new());
    let context_id = ContextId::new(1000, 96);
    let metadata = ToolEffectMetadata {
        tool_name: "leak_tool".to_string(),
        function_name: None,
        args: json!({}),
        metadata: json!({"message_id": "msg-leak"}),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };
    let token = bus
        .as_ref()
        .start_tool(context_id.clone(), metadata)
        .await
        .unwrap();
    let counts = bus.in_flight(&context_id).await;
    assert_eq!(counts.tool, 1, "Started: in-flight 1");
    drop(token); // Intentionally leak: no complete()
    let counts = bus.in_flight(&context_id).await;
    assert_eq!(
        counts.tool, 1,
        "CG3: Dropped token leaves in-flight at 1 (leak)"
    );
}

// Property test: Effect start/complete pairing and underflow (I1, I3, E2).
// ops: 0 = Start, 1 = Complete (if any started), 2 = Orphan Complete (bus applies saturating_sub; order-dependent).
// Expected in_flight is computed by the same semantics as BusWithEffects: running count, +1 on Start, saturating_sub(1) on Completed.
proptest! {
    #![proptest_config(proptest_cfg(16))]
    #[test]
    fn prop_effect_pairing_and_underflow(ops in proptest::collection::vec(0u8..3, 0..12)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let bus = Arc::new(BusWithEffects::new());
            let context_id = ContextId::new(1000, 100);
            let mut started_count = 0u32;
            let mut completed_count = 0u32;
            let mut running = 0i64;

            for (i, op) in ops.iter().copied().enumerate() {
                match op {
                    0 => {
                        let metadata = ToolEffectMetadata {
                            tool_name: format!("tool_{}", i),
                            function_name: None,
                            args: json!({}),
                            metadata: json!({"message_id": format!("msg-{}", i)}),
                            delegation_target: None,
                            tool_backend: None,
                            tool_digest: None,
                        };
                        bus.emit(EffectEvent::ToolStarted {
                            context_id: context_id.clone(),
                            metadata: metadata.clone(),
                        })
                        .await
                        .unwrap();
                        started_count += 1;
                        running = (running + 1).max(0);
                    }
                    1 => {
                        if started_count > completed_count {
                            let metadata = ToolEffectMetadata {
                                tool_name: format!("tool_{}", i),
                                function_name: None,
                                args: json!({}),
                                metadata: json!({"message_id": format!("msg-{}", i)}),
                                delegation_target: None,
                                tool_backend: None,
                                tool_digest: None,
                            };
                            bus.emit(EffectEvent::ToolCompleted {
                                context_id: context_id.clone(),
                                metadata,
                                duration_ms: 100,
                                outcome: Outcome::Success,
                            result: None,
        })
                            .await
                            .unwrap();
                            completed_count += 1;
                            running = (running - 1).max(0);
                        }
                    }
                    _ => {
                        let metadata = ToolEffectMetadata {
                            tool_name: format!("orphan_{}", i),
                            function_name: None,
                            args: json!({}),
                            metadata: json!({"message_id": format!("msg-orphan-{}", i)}),
                            delegation_target: None,
                            tool_backend: None,
                            tool_digest: None,
                        };
                        bus.emit(EffectEvent::ToolCompleted {
                            context_id: context_id.clone(),
                            metadata,
                            duration_ms: 0,
                            outcome: Outcome::Success,
                        result: None,
        })
                        .await
                        .unwrap();
                        running = (running - 1).max(0);
                    }
                }
            }

            let counts = bus.in_flight(&context_id).await;
            let expected = running.max(0) as u32;
            assert_eq!(
                counts.tool,
                expected,
                "In-flight must match bus semantics (Start +1, Completed saturating_sub 1)"
            );
        });
    }
}
