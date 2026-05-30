// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Promise resolution polling for `evaluate()`.
//!
//! When JS returns a promise, the host runs pending jobs and checks
//! `__eval_result` in a loop until the promise resolves or timeout.
//! Effect-gated timeout (L5–L6) distinguishes "waiting on effect" from
//! "will never yield". See this crate's `README.md` for current liveness architecture.
//!
//! **Deadlock avoidance:** (1) The poll loop must not hold the bridge lock across awaits.
//! (2) `exe_rt_task_in_event_loop` is synchronous so we run it in `spawn_blocking`.
//! **Ordering:** When `result_notify` is set (resume path), we wait on it so we observe the
//! result only after __set_eval_result has run; no reliance on event-loop task ordering.
//!
//! **L4-Resume:** We drive the QuickJS worker at the start of each loop iteration.
//! BAML completion posts the resolve via `add_rt_task_to_event_loop_void`; the continuation
//! (`.then(__set_eval_result)`) only runs when we call `run_pending_jobs_if_any()`
//! on the same QuickJS runtime (single worker).

use std::{pin::Pin, sync::Arc};

use baml_rt_core::{
    BamlRtError, Result,
    bus::EffectLiveness,
    context::{InvocationScope, RuntimeScope},
};
use dashmap::DashMap;
use quickjs_runtime::facades::QuickJsRuntimeFacade;

use super::scope::InvocationToken;
use crate::quickjs_bridge::eval::EffectGatedTimeoutPolicy;

type EvalResultMap = Arc<DashMap<InvocationToken, Option<String>>>;
type InvocationScopeMap = Arc<DashMap<InvocationToken, RuntimeScope>>;

/// When set, the poll loop uses this each iteration instead of `runtime` so the bridge
/// lock is held only briefly (lock → run pending jobs → unlock) and never across an await.
pub(crate) type RunPendingJobsBrief =
    Arc<dyn Fn() -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Re-check effect-gated timeout every N attempts after the initial phase.
const EFFECT_CHECK_INTERVAL: u32 = 100;
/// For the first EFFECT_EARLY_CHECK_WINDOW attempts, re-check every N so we see effects soon.
const EFFECT_EARLY_CHECK_INTERVAL: u32 = 10;
const EFFECT_EARLY_CHECK_WINDOW: u32 = 500;
/// For the first N attempts, never use the short idle timeout so the promise executor has time
/// to run and emit effects on slow CI (where run_pending_jobs may be scheduled late).
const EFFECT_WARMUP_ATTEMPTS: u32 = 2000;
/// Absolute cap on polling attempts (~30 s at 1 ms/sleep). Prevents unbounded spin when
/// effect-gated policy keeps extending. Also surfaces deadlock when deliver_resume_input
/// calls invoke_js_function → evaluate() → poll: the bridge lock is held across the poll
/// so the LLM completion task cannot acquire it to resolve the promise (ctx-1-2 stall).
const ABSOLUTE_CAP_ATTEMPTS: u32 = 30_000;

/// Parameters for promise resolution polling (keeps `poll_promise_until_result` under clippy's arg limit).
///
/// Use `runtime: Some(arc)` so the poll loop runs pending jobs without holding the bridge lock.
/// When `result_notify` is Some (resume path), the loop waits on it so the result is observed
/// only after __set_eval_result has run (strict ordering; no event-loop race).
pub(crate) struct PollPromiseParams<'a> {
    /// When Some, used each iteration to run pending jobs without holding bridge lock.
    pub runtime: Option<Arc<QuickJsRuntimeFacade>>,
    pub eval_results_by_token: &'a EvalResultMap,
    pub eval_token: &'a InvocationToken,
    pub token_to_remove: Option<&'a InvocationToken>,
    pub invocation_scope_by_token: &'a InvocationScopeMap,
    pub scope: &'a InvocationScope,
    pub effect_liveness: Option<Arc<dyn EffectLiveness>>,
    pub idle_timeout_ms: u64,
    pub max_attempts_ms: u64,
    /// When Some, used each iteration instead of `runtime` (legacy brief-lock path).
    pub run_pending_jobs_brief: Option<RunPendingJobsBrief>,
    /// When Some, poll loop waits on this before checking result so we observe only after __set_eval_result (no ordering race).
    pub result_notify: Option<Arc<tokio::sync::Notify>>,
}

/// Poll until `__eval_result` is set for the given token or timeout.
///
/// Runs `runtime.run_pending_jobs_if_any()` each iteration so promise
/// continuations can run. Uses effect-gated timeout when effect-liveness
/// wiring is available; otherwise falls back to the configured max timeout.
/// When the loop
/// exits (success or timeout), removes the invocation token from
/// `invocation_scope_by_token` if `token_to_remove` is `Some`.
pub(crate) async fn poll_promise_until_result(params: PollPromiseParams<'_>) -> Result<String> {
    let PollPromiseParams {
        runtime,
        eval_results_by_token,
        eval_token,
        token_to_remove,
        invocation_scope_by_token,
        scope,
        effect_liveness,
        idle_timeout_ms,
        max_attempts_ms,
        run_pending_jobs_brief,
        result_notify,
    } = params;

    let poller = effect_liveness.map(|liveness| {
        EffectGatedTimeoutPolicy::new(
            liveness,
            scope.context_id().clone(),
            idle_timeout_ms,
            max_attempts_ms,
        )
    });
    let mut timeout_attempts: Option<u32> = None;
    let mut attempts = 0u32;
    let is_resume_poll = result_notify.is_some();

    // .entered() is !Send (holds a raw pointer) — cannot be used here because the future
    // must be Send for tokio::spawn. Use tracing events directly; callers that need span
    // attribution can wrap this future with .instrument(span) at the call site.
    tracing::debug!(
        token = %eval_token.0,
        context_id = %scope.context_id(),
        resume_poll = is_resume_poll,
        "poll_promise: start"
    );

    /// Run pending jobs (spawn_blocking or run_brief) so the worker can make progress.
    async fn run_pending_jobs_once(
        runtime: &Option<Arc<QuickJsRuntimeFacade>>,
        run_pending_jobs_brief: &Option<RunPendingJobsBrief>,
    ) -> Result<()> {
        if let Some(run_brief) = run_pending_jobs_brief {
            run_brief().await;
        } else if let Some(rt) = runtime {
            let rt_clone = rt.clone();
            tokio::task::spawn_blocking(move || {
                rt_clone.exe_rt_task_in_event_loop(|r| r.run_pending_jobs_if_any());
            })
            .await
            .map_err(|e| BamlRtError::QuickJs(format!("spawn_blocking join: {}", e)))?;
        } else {
            return Err(BamlRtError::QuickJs(
                "poll_promise_until_result: either runtime or run_pending_jobs_brief must be set"
                    .to_string(),
            ));
        }
        Ok(())
    }

    const TICK_SLEEP_MS: u64 = 20;

    loop {
        // Drive the worker first so the continuation (__set_eval_result) can run before we wait.
        // With a single QuickJS runtime, the resolve is posted to this worker; we must run
        // run_pending_jobs_if_any() for it (and the .then callback) to run.
        run_pending_jobs_once(&runtime, &run_pending_jobs_brief).await?;
        tokio::task::yield_now().await;

        if let Some(notify) = result_notify.as_ref() {
            if attempts > 0 && attempts.is_multiple_of(500) {
                tracing::debug!(token = %eval_token.0, attempts, "poll: still waiting for result_notify or drain");
            }
            tokio::select! {
                biased;
                _ = notify.notified() => {
                    tracing::debug!(token = %eval_token.0, "poll: notify fired");
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(TICK_SLEEP_MS)) => {}
            }
        }
        if attempts > 0 && attempts.is_multiple_of(250) {
            tracing::trace!(
                token = %eval_token.0,
                attempts,
                timeout_attempts = ?timeout_attempts,
                resume_poll = is_resume_poll,
                "poll_promise: interleaving checkpoint"
            );
        }

        let result_str = match eval_results_by_token.get_mut(eval_token) {
            Some(mut slot) => slot.take(),
            None => {
                return Err(BamlRtError::QuickJs(
                    "Missing eval result slot for token".to_string(),
                ));
            }
        };

        if let Some(result_str) = result_str {
            if let Some(t) = token_to_remove {
                invocation_scope_by_token.remove(t);
            }
            eval_results_by_token.remove(eval_token);
            tracing::trace!(attempts = attempts, "Promise resolved");
            return Ok(result_str);
        }

        // First iteration or periodic re-check: sample effect-gated timeout. Early window
        // uses a shorter interval so we see effects soon after they start.
        let should_recheck = if timeout_attempts.is_none() {
            true
        } else if attempts < EFFECT_EARLY_CHECK_WINDOW {
            attempts > 0 && attempts.is_multiple_of(EFFECT_EARLY_CHECK_INTERVAL)
        } else {
            attempts.is_multiple_of(EFFECT_CHECK_INTERVAL)
        };
        if should_recheck && let Some(poller) = poller.as_ref() {
            let new_timeout = poller.timeout_attempts().await;
            tracing::trace!(
                attempts,
                context_id = %scope.context_id(),
                timeout_attempts = new_timeout,
                "poll_promise: effect-gated timeout sample"
            );
            timeout_attempts = Some(timeout_attempts.map_or(new_timeout, |t| t.max(new_timeout)));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        attempts += 1;
        let mut limit = timeout_attempts.unwrap_or(u32::MAX);
        if attempts < EFFECT_WARMUP_ATTEMPTS {
            limit = limit.max(max_attempts_ms as u32);
        }
        // Hard cap so we never spin unbounded (e.g. promise never resolves for stream/nested ctx).
        limit = limit.min(ABSOLUTE_CAP_ATTEMPTS);
        if attempts >= limit {
            tracing::warn!(
                attempts,
                context_id = %scope.context_id(),
                limit,
                "Promise did not resolve within cap; possible deadlock or unresolved stream/nested eval"
            );
            if let Some(t) = token_to_remove {
                invocation_scope_by_token.remove(t);
            }
            eval_results_by_token.remove(eval_token);
            return Err(BamlRtError::QuickJs(format!(
                "Promise did not resolve after {} attempts ({}ms)",
                limit, limit
            )));
        }
    }
}
