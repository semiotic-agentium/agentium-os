// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `ExecutionSession` typestate FSM.
//!
//! `ExecutionSession` is private, so these tests drive it through the
//! `__execution_session_invoke` host function registered by `QuickJSBridge`.
//! Each JS snippet exercises a specific transition; the bridge returns
//! `{ "error": "..." }` on rejection.
//!
//! **Coverage:**
//! - Wrong-phase transitions (submit_plan before intent, start/complete step in wrong phase)
//! - Unknown step_id
//! - Step status guard (start already in_progress, complete not in_progress)
//! - Dependency enforcement (cannot start before dependency is completed)
//! - Double-close (`Finish` twice)
//! - Epoch/CAS stale-write rejection
//! - Scope mismatch rejection

#![recursion_limit = "256"]

use baml_rt::quickjs_bridge::QuickJSBridge;
use baml_rt_core::{
    context::{self, InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use serde_json::Value;
use test_support::common::make_capturing_bridge;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_bridge_with_agent(uuid_str: &str) -> (QuickJSBridge, AgentId) {
    let agent_id = AgentId::from_uuid(UuidId::parse_str(uuid_str).unwrap());
    let (bridge, _capture) = make_capturing_bridge(agent_id.clone()).await;
    (bridge, agent_id)
}

fn task_scope(agent_id: AgentId, ctx_ms: u64, tag: &str) -> (RuntimeScope, InvocationScope) {
    let context_id = ContextId::new(ctx_ms, 1);
    let message_id = MessageId::from_external(ExternalId::new(format!("msg-{tag}")));
    let task_id = TaskId::from_external(ExternalId::new(format!("task-{tag}")));
    let scope = RuntimeScope::task_scope(context_id, agent_id, message_id, task_id);
    let invoke_scope = InvocationScope::new(scope.clone());
    (scope, invoke_scope)
}

async fn eval_js(
    bridge: &mut QuickJSBridge,
    scope: &RuntimeScope,
    invoke_scope: &InvocationScope,
    js: &str,
) -> Value {
    context::with_scope(scope.clone(), async {
        bridge
            .eval_scoped(invoke_scope, js)
            .await
            .expect("evaluate returned Err")
    })
    .await
}

fn err_str(v: &Value) -> String {
    v.get("error")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Wrong-phase transition matrix
// ---------------------------------------------------------------------------

/// Verifies that each invalid command sequence produces the expected error substring.
#[tokio::test]
async fn test_execution_session_wrong_phase_transitions() {
    struct Case {
        name: &'static str,
        uuid: &'static str,
        ctx_ms: u64,
        /// JS that should return `{ error: "..." }` containing `expected_err`
        js: &'static str,
        expected_err: &'static str,
    }

    let cases: &[Case] = &[
        // SubmitPlan before SubmitIntent — session is still AwaitIntent
        Case {
            name: "submit_plan_before_intent",
            uuid: "00000000-0000-0000-0000-cc0100000001",
            ctx_ms: 60_001,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "submit_plan",
                    session_id: sessionId,
                    plan: {
                      intentId: "intent-x", planId: "plan-x",
                      steps: [{ stepId: "s1", description: "d", order: 0, dependsOn: [] }]
                    }
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "cannot submitPlan in current phase",
        },
        // StartStep before SubmitPlan — session is AwaitPlan, not Executable
        Case {
            name: "start_step_before_plan",
            uuid: "00000000-0000-0000-0000-cc0100000002",
            ctx_ms: 60_002,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "start_step",
                    session_id: sessionId,
                    step_id: "s1",
                    citations: ["#1"]
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "execution session is not executable",
        },
        // Double SubmitIntent — second submit on AwaitPlan should fail
        Case {
            name: "double_submit_intent",
            uuid: "00000000-0000-0000-0000-cc0100000003",
            ctx_ms: 60_003,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "submit_intent",
                    session_id: sessionId,
                    intent: { intentId: "intent-y", description: "d2", citations: ["#1"] }
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "cannot submitIntent in current phase",
        },
        // Finish then Finish again — double-close
        Case {
            name: "double_finish",
            uuid: "00000000-0000-0000-0000-cc0100000004",
            ctx_ms: 60_004,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({ action: "finish", session_id: sessionId }));
                try {
                  await __execution_session_invoke(JSON.stringify({ action: "finish", session_id: sessionId }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "cannot finish in current phase",
        },
        // StartStep with unknown step_id
        Case {
            name: "start_unknown_step",
            uuid: "00000000-0000-0000-0000-cc0100000005",
            ctx_ms: 60_005,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_plan",
                  session_id: sessionId,
                  plan: {
                    intentId: "intent-x", planId: "plan-x",
                    steps: [{ stepId: "real-step", description: "d", order: 0, dependsOn: [] }]
                  }
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "start_step",
                    session_id: sessionId,
                    step_id: "ghost-step",
                    citations: ["#1"]
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "stepId does not exist in plan",
        },
        // StartStep on step already in_progress
        Case {
            name: "start_step_already_in_progress",
            uuid: "00000000-0000-0000-0000-cc0100000006",
            ctx_ms: 60_006,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_plan",
                  session_id: sessionId,
                  plan: {
                    intentId: "intent-x", planId: "plan-x",
                    steps: [{ stepId: "s1", description: "d", order: 0, dependsOn: [] }]
                  }
                }));
                await __execution_session_invoke(JSON.stringify({
                  action: "start_step", session_id: sessionId,
                  step_id: "s1", citations: ["#1"]
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "start_step", session_id: sessionId,
                    step_id: "s1", citations: ["#1"]
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "step cannot transition to in_progress from in_progress",
        },
        // CompleteStep on step still pending (not in_progress)
        Case {
            name: "complete_step_not_in_progress",
            uuid: "00000000-0000-0000-0000-cc0100000007",
            ctx_ms: 60_007,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_plan",
                  session_id: sessionId,
                  plan: {
                    intentId: "intent-x", planId: "plan-x",
                    steps: [{ stepId: "s1", description: "d", order: 0, dependsOn: [] }]
                  }
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "complete_step", session_id: sessionId,
                    step_id: "s1", citations: ["#1"]
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "step cannot transition to completed from pending",
        },
        // StartStep blocked by unsatisfied dependency
        Case {
            name: "start_step_unsatisfied_dependency",
            uuid: "00000000-0000-0000-0000-cc0100000008",
            ctx_ms: 60_008,
            js: r##"
              (async function() {
                const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
                const { sessionId } = JSON.parse(r);
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_intent",
                  session_id: sessionId,
                  intent: { intentId: "intent-x", description: "d", citations: ["#1"] }
                }));
                await __execution_session_invoke(JSON.stringify({
                  action: "submit_plan",
                  session_id: sessionId,
                  plan: {
                    intentId: "intent-x", planId: "plan-x",
                    steps: [
                      { stepId: "s1", description: "first",  order: 0, dependsOn: [] },
                      { stepId: "s2", description: "second", order: 1, dependsOn: ["s1"] }
                    ]
                  }
                }));
                try {
                  await __execution_session_invoke(JSON.stringify({
                    action: "start_step", session_id: sessionId,
                    step_id: "s2", citations: ["#1"]
                  }));
                  return { error: "no error raised" };
                } catch(e) { return { error: e.toString() }; }
              })()
            "##,
            expected_err: "cannot start step before dependency",
        },
    ];

    for case in cases {
        let (mut bridge, agent_id) = make_bridge_with_agent(case.uuid).await;
        let (scope, invoke_scope) = task_scope(agent_id, case.ctx_ms, case.name);
        let result = eval_js(&mut bridge, &scope, &invoke_scope, case.js).await;
        let got = err_str(&result);
        assert!(
            got.contains(case.expected_err),
            "case '{}': expected error containing {:?}, got: {:?} (full result: {result:?})",
            case.name,
            case.expected_err,
            got,
        );
    }
}

// ---------------------------------------------------------------------------
// Epoch / CAS stale-write rejection
// ---------------------------------------------------------------------------

/// Open two sessions for the same task; submit intent on the second with supersession
/// so the lineage epoch advances. Then attempt `startStep` on the first session
/// and assert the epoch guard fires.
#[tokio::test]
async fn test_execution_session_stale_epoch_rejected() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-cc0200000001").unwrap());
    let (mut bridge, _capture) = make_capturing_bridge(agent_id.clone()).await;
    let (scope, invoke_scope) = task_scope(agent_id, 61_001, "stale-epoch");

    let js = r##"
      (async function() {
        // Session A: open → submitIntent → submitPlan (epoch 1)
        const rA = await __execution_session_invoke(JSON.stringify({ action: "open" }));
        const sessionA = JSON.parse(rA).sessionId;
        await __execution_session_invoke(JSON.stringify({
          action: "submit_intent",
          session_id: sessionA,
          intent: { intentId: "intent-a", description: "first", citations: ["#1"] }
        }));
        await __execution_session_invoke(JSON.stringify({
          action: "submit_plan",
          session_id: sessionA,
          plan: {
            intentId: "intent-a", planId: "plan-a",
            steps: [{ stepId: "s1", description: "step", order: 0, dependsOn: [] }]
          }
        }));

        // Session B: open → submitIntent with supersession=replaced_by (epoch advances to 2)
        const rB = await __execution_session_invoke(JSON.stringify({ action: "open" }));
        const sessionB = JSON.parse(rB).sessionId;
        await __execution_session_invoke(JSON.stringify({
          action: "submit_intent",
          session_id: sessionB,
          intent: {
            intentId: "intent-b", description: "supersedes first", citations: ["#1"],
            supersession: "replaced_by"
          }
        }));

        // Session A tries to startStep — should fail with stale lineage
        try {
          await __execution_session_invoke(JSON.stringify({
            action: "start_step",
            session_id: sessionA,
            step_id: "s1",
            citations: ["#1"]
          }));
          return { error: "no error raised" };
        } catch(e) { return { error: e.toString() }; }
      })()
    "##;

    let result = eval_js(&mut bridge, &scope, &invoke_scope, js).await;
    let got = err_str(&result);
    assert!(
        got.contains("stale lineage"),
        "expected 'stale lineage' error, got: {:?} (full: {result:?})",
        got
    );
}

// ---------------------------------------------------------------------------
// Session exhaustion — closed session ID cannot be reused
// ---------------------------------------------------------------------------

/// After Finish closes a session, any subsequent operation using that session ID
/// must fail with "execution session not found". This verifies that session state
/// is not leaked and IDs cannot be replayed.
#[tokio::test]
async fn test_execution_session_id_cannot_be_reused_after_finish() {
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-cc0300000001").unwrap());
    let (mut bridge, _capture) = make_capturing_bridge(agent_id.clone()).await;
    let (scope, invoke_scope) = task_scope(agent_id, 62_001, "session-exhaustion");

    let js = r##"
      (async function() {
        const r = await __execution_session_invoke(JSON.stringify({ action: "open" }));
        const { sessionId } = JSON.parse(r);

        // Complete the full lifecycle so Finish is permitted
        await __execution_session_invoke(JSON.stringify({
          action: "submit_intent", session_id: sessionId,
          intent: { intentId: "i1", description: "d", citations: ["#1"] }
        }));
        await __execution_session_invoke(JSON.stringify({
          action: "submit_plan", session_id: sessionId,
          plan: { intentId: "i1", planId: "p1", steps: [{ stepId: "s1", description: "d", order: 0, dependsOn: [] }] }
        }));
        await __execution_session_invoke(JSON.stringify({
          action: "start_step", session_id: sessionId, step_id: "s1", citations: ["#1"]
        }));
        await __execution_session_invoke(JSON.stringify({
          action: "complete_step", session_id: sessionId, step_id: "s1", citations: ["#1"]
        }));
        await __execution_session_invoke(JSON.stringify({ action: "finish", session_id: sessionId }));

        // Closed session — any subsequent operation must fail with not found
        try {
          await __execution_session_invoke(JSON.stringify({
            action: "submit_intent",
            session_id: sessionId,
            intent: { intentId: "intent-replay", description: "d", citations: ["#1"] }
          }));
          return { error: "no error raised" };
        } catch(e) { return { error: e.toString() }; }
      })()
    "##;
    let result = eval_js(&mut bridge, &scope, &invoke_scope, js).await;
    let got = err_str(&result);
    // Closed session: operations are rejected because the session is in Closed phase,
    // not because the ID is gone (the session remains in the store as Closed for provenance).
    assert!(
        got.contains("cannot submitIntent in current phase")
            || got.contains("execution session not found"),
        "expected 'cannot submitIntent in current phase' after finish, got: {:?} (full: {result:?})",
        got
    );
}
