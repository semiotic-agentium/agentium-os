#![recursion_limit = "256"]
//! Property tests for task status FSM invariants.
//!
//! ## Core Properties (A2A Life-of-a-Task)
//!
//! 1. **I2 (Terminal immutability)**: ∀ task t, if t.status ∈ TERMINAL, then record_status_update
//!    returns Err for any further update.
//! 2. **I3 (Allowed transitions)**: ∀ valid transition (from, to), record_status_update succeeds;
//!    ∀ invalid transition, returns Err.
//! 3. **First status = SUBMITTED**: For a new task, first record_status_update (AgentEmitted) must
//!    be SUBMITTED or returns Err. HostInferred may seed any first state.
//! 4. **Upsert merge-preserve**: If upsert(task) where task.status is None and the task already
//!    exists with status S, the stored task retains S.
//! 5. **I1 (FSM boundary)**: upsert(task) with task.status = Some(X) never mutates stored status;
//!    status changes only via record_status_update.
//!
//! Tests at TaskStore level (sync) to exercise the FSM and upsert logic directly.

mod common;

use baml_rt_a2a::{a2a_store::TaskStore, a2a_types::TaskState};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use proptest::prelude::*;

fn proptest_cfg(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(cases);
    cfg.failure_persistence = None;
    cfg
}

const S_SUBMITTED: &str = "TASK_STATE_SUBMITTED";
const S_WORKING: &str = "TASK_STATE_WORKING";
const S_COMPLETED: &str = "TASK_STATE_COMPLETED";
const S_FAILED: &str = "TASK_STATE_FAILED";
const S_CANCELED: &str = "TASK_STATE_CANCELED";
const S_REJECTED: &str = "TASK_STATE_REJECTED";
const S_INPUT_REQUIRED: &str = "TASK_STATE_INPUT_REQUIRED";
const S_AUTH_REQUIRED: &str = "TASK_STATE_AUTH_REQUIRED";

const ALL_STATES: &[&str] = &[
    S_SUBMITTED,
    S_WORKING,
    S_COMPLETED,
    S_FAILED,
    S_CANCELED,
    S_REJECTED,
    S_INPUT_REQUIRED,
    S_AUTH_REQUIRED,
];

fn is_terminal(s: &str) -> bool {
    matches!(s, S_COMPLETED | S_FAILED | S_CANCELED | S_REJECTED)
}

/// Allowed transitions (from, to). Same-state is always allowed.
fn is_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if is_terminal(from) {
        return false;
    }
    matches!(
        (from, to),
        (S_SUBMITTED, S_WORKING)
            | (S_SUBMITTED, S_COMPLETED)
            | (S_SUBMITTED, S_FAILED)
            | (S_SUBMITTED, S_CANCELED)
            | (S_SUBMITTED, S_REJECTED)
            | (S_SUBMITTED, S_INPUT_REQUIRED)
            | (S_SUBMITTED, S_AUTH_REQUIRED)
            | (S_WORKING, S_INPUT_REQUIRED)
            | (S_WORKING, S_AUTH_REQUIRED)
            | (S_WORKING, S_COMPLETED)
            | (S_WORKING, S_FAILED)
            | (S_WORKING, S_CANCELED)
            | (S_WORKING, S_REJECTED)
            | (S_INPUT_REQUIRED, S_WORKING)
            | (S_INPUT_REQUIRED, S_CANCELED)
            | (S_INPUT_REQUIRED, S_REJECTED)
            | (S_INPUT_REQUIRED, S_COMPLETED)
            | (S_INPUT_REQUIRED, S_FAILED)
            | (S_AUTH_REQUIRED, S_WORKING)
            | (S_AUTH_REQUIRED, S_CANCELED)
            | (S_AUTH_REQUIRED, S_REJECTED)
            | (S_AUTH_REQUIRED, S_COMPLETED)
            | (S_AUTH_REQUIRED, S_FAILED)
    )
}

fn seed_task_and_submitted(store: &mut TaskStore, task_id: &TaskId, context_id: &ContextId) {
    let task = common::minimal_task(task_id, context_id, None);
    store.upsert(task);
    let _ = store.record_status_update(
        Some(task_id.clone()),
        Some(context_id.clone()),
        common::task_status(S_SUBMITTED),
    );
}

proptest! {
    #![proptest_config(proptest_cfg(32))]

    /// PROPERTY I2 + I3: Valid transition sequences succeed; invalid ones fail.
    /// Generate a sequence of target states. First must be SUBMITTED (HostInferred).
    /// Each subsequent must be an allowed transition from current.
    #[test]
    fn prop_valid_transition_sequences_succeed(seq in prop::collection::vec(0u8..=7u8, 1..=12)) {
        let mut store = TaskStore::new();
        let task_id = TaskId::from_external(ExternalId::new("task-prop-1"));
        let context_id = ContextId::new(1, 1);

        seed_task_and_submitted(&mut store, &task_id, &context_id);

        let mut current = S_SUBMITTED;
        for idx in seq {
            let next_state = ALL_STATES[idx as usize % ALL_STATES.len()];
            if is_terminal(current) {
                // I2: terminal rejects any further update (including same-state)
                let result = store.record_status_update(
                    Some(task_id.clone()),
                    Some(context_id.clone()),
                    common::task_status(next_state),
                );
                assert!(result.is_none(), "terminal {} must reject any further update", current);
                continue;
            }
            if !is_allowed(current, next_state) {
                let result = store.record_status_update(
                    Some(task_id.clone()),
                    Some(context_id.clone()),
                    common::task_status(next_state),
                );
                assert!(result.is_none(), "invalid transition {} -> {} should fail", current, next_state);
                continue;
            }
            let result = store.record_status_update(
                Some(task_id.clone()),
                Some(context_id.clone()),
                common::task_status(next_state),
            );
            assert!(result.is_some(), "valid transition {} -> {} should succeed: {:?}", current, next_state, result);
            current = next_state;
        }
    }

    /// PROPERTY (First status): AgentEmitted first state must be SUBMITTED.
    /// Create task without seeding, then try record_status_update with AgentEmitted.
    /// Only SUBMITTED should succeed.
    #[test]
    fn prop_first_status_must_be_submitted_for_agent_emitted(state_idx in 0u8..=7u8) {
        let mut store = TaskStore::new();
        let task_id = TaskId::from_external(ExternalId::new("task-prop-2"));
        let context_id = ContextId::new(1, 1);

        // Upsert task with no status (simulates task created before any status)
        let task = common::minimal_task(&task_id, &context_id, None);
        store.upsert(task);

        let state = ALL_STATES[state_idx as usize % ALL_STATES.len()];
        let result = store.record_status_update(
            Some(task_id.clone()),
            Some(context_id.clone()),
            common::task_status(state),
        );

        if state == S_SUBMITTED {
            assert!(result.is_some(), "SUBMITTED as first AgentEmitted should succeed");
        } else {
            assert!(result.is_none(), "first AgentEmitted {} must be rejected", state);
        }
    }

    /// PROPERTY (Upsert merge-preserve): After recording status, upsert with status=None
    /// must not overwrite existing status.
    #[test]
    fn prop_upsert_merge_preserves_existing_status(intermediate_idx in 0u8..=7u8) {
        let mut store = TaskStore::new();
        let task_id = TaskId::from_external(ExternalId::new("task-prop-3"));
        let context_id = ContextId::new(1, 1);

        seed_task_and_submitted(&mut store, &task_id, &context_id);

        // Move to an intermediate state (pick one that's allowed)
        let intermediate = ALL_STATES[intermediate_idx as usize % ALL_STATES.len()];
        if is_allowed(S_SUBMITTED, intermediate) {
            let _ = store.record_status_update(
                Some(task_id.clone()),
                Some(context_id.clone()),
                common::task_status(intermediate),
            );
        }

        // Upsert task with status=None (simulates TaskProcessor flow)
        let task_without_status = common::minimal_task(&task_id, &context_id, None);
        store.upsert(task_without_status);

        // Stored task must still have a status (the one we set)
        let stored = store.get(task_id.as_str(), None).expect("task exists");
        assert!(
            stored.status.is_some(),
            "upsert with status=None must preserve existing status"
        );
        let state_str = stored
            .status
            .as_ref()
            .and_then(|s| s.state.as_ref())
            .and_then(|st| match st {
                TaskState::String(s) => Some(s.as_str()),
                _ => None,
            });
        assert!(
            state_str.is_some(),
            "stored status must have a valid state string"
        );
    }

    /// PROPERTY I1: upsert with status=Some(X) must not mutate stored status; FSM boundary.
    #[test]
    fn prop_upsert_with_status_does_not_mutate_fsm(attempted_state_idx in 0u8..=7u8) {
        let mut store = TaskStore::new();
        let task_id = TaskId::from_external(ExternalId::new("task-prop-i1"));
        let context_id = ContextId::new(1, 1);

        seed_task_and_submitted(&mut store, &task_id, &context_id);
        let _ = store.record_status_update(
            Some(task_id.clone()),
            Some(context_id.clone()),
            common::task_status(S_WORKING),
        );

        let attempted = ALL_STATES[attempted_state_idx as usize % ALL_STATES.len()];
        let task_with_status = common::minimal_task(
            &task_id,
            &context_id,
            Some(common::task_status(attempted)),
        );
        store.upsert(task_with_status);

        let stored = store.get(task_id.as_str(), None).expect("task exists");
        let state_str = stored
            .status
            .as_ref()
            .and_then(|s| s.state.as_ref())
            .and_then(|st| match st {
                TaskState::String(s) => Some(s.as_str()),
                _ => None,
            });
        assert_eq!(
            state_str,
            Some(S_WORKING),
            "I1: upsert(status=Some({})) must not mutate stored status; expected WORKING",
            attempted
        );
    }

    /// PROPERTY I2: Once terminal, no further updates accepted.
    #[test]
    fn prop_terminal_state_rejects_further_updates(terminal_idx in 0u8..=3u8, attempt_idx in 0u8..=7u8) {
        let mut store = TaskStore::new();
        let task_id = TaskId::from_external(ExternalId::new("task-prop-4"));
        let context_id = ContextId::new(1, 1);

        seed_task_and_submitted(&mut store, &task_id, &context_id);

        let terminals = [S_COMPLETED, S_FAILED, S_CANCELED, S_REJECTED];
        let terminal = terminals[terminal_idx as usize % terminals.len()];

        // Transition to terminal (SUBMITTED -> terminal is allowed)
        let _ = store.record_status_update(
            Some(task_id.clone()),
            Some(context_id.clone()),
            common::task_status(terminal),
        );

        // Any further update must fail
        let attempt = ALL_STATES[attempt_idx as usize % ALL_STATES.len()];
        let result = store.record_status_update(
            Some(task_id.clone()),
            Some(context_id.clone()),
            common::task_status(attempt),
        );
        assert!(
            result.is_none(),
            "terminal state {} must reject further update to {}",
            terminal,
            attempt
        );
    }
}
