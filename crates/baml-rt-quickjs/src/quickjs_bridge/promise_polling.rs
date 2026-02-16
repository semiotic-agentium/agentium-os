//! Promise resolution polling for `evaluate()`.
//!
//! When JS returns a promise, the host runs pending jobs and checks
//! `__eval_result` in a loop until the promise resolves or timeout.
//! Effect-gated timeout (L5–L6) distinguishes "waiting on effect" from
//! "will never yield". See docs/HOST_QUICKJS_STREAM_INVARIANTS.md.

use crate::quickjs_bridge::eval::EffectGatedTimeoutPolicy;
use baml_rt_core::bus::EffectLiveness;
use baml_rt_core::context::{InvocationScope, RuntimeScope};
use baml_rt_core::{BamlRtError, Result};
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use super::scope::InvocationToken;

type EvalResultMap = Arc<StdMutex<HashMap<InvocationToken, Option<String>>>>;
type InvocationScopeMap = Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>;

/// Re-check effect-gated timeout every N attempts after the initial phase.
const EFFECT_CHECK_INTERVAL: u32 = 100;
/// For the first EFFECT_EARLY_CHECK_WINDOW attempts, re-check every N so we see effects soon.
const EFFECT_EARLY_CHECK_INTERVAL: u32 = 10;
const EFFECT_EARLY_CHECK_WINDOW: u32 = 500;

/// Parameters for promise resolution polling (keeps `poll_promise_until_result` under clippy's arg limit).
pub(crate) struct PollPromiseParams<'a> {
    pub runtime: &'a QuickJsRuntimeFacade,
    pub eval_results_by_token: &'a EvalResultMap,
    pub eval_token: &'a InvocationToken,
    pub token_to_remove: Option<&'a InvocationToken>,
    pub invocation_scope_by_token: &'a InvocationScopeMap,
    pub scope: &'a InvocationScope,
    pub effect_liveness: Arc<dyn EffectLiveness>,
    pub idle_timeout_ms: u64,
    pub max_attempts_ms: u64,
}

/// Poll until `__eval_result` is set for the given token or timeout.
///
/// Runs `runtime.run_pending_jobs_if_any()` each iteration so promise
/// continuations can run. Uses effect-gated timeout: long timeout when
/// effects are in-flight, short idle timeout otherwise. Requires both
/// invocation scope and effect-liveness wiring.
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
    } = params;

    let poller = EffectGatedTimeoutPolicy::new(
        effect_liveness,
        scope.context_id().clone(),
        idle_timeout_ms,
        max_attempts_ms,
    );
    // Sample timeout only after the first run of pending jobs so the promise executor has had
    // one chance to run and emit effects; then re-check frequently early so we see effects
    // as soon as they appear (deterministic policy, no magic iteration count).
    let mut timeout_attempts: Option<u32> = None;
    let mut attempts = 0u32;

    let _poll_guard = tracing::trace_span!("baml_rt.poll_promise_resolution").entered();

    loop {
        runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });
        tokio::task::yield_now().await;

        let result_str = {
            let mut guard = eval_results_by_token
                .lock()
                .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
            match guard.get_mut(eval_token) {
                Some(slot) => slot.take(),
                None => {
                    return Err(BamlRtError::QuickJs(
                        "Missing eval result slot for token".to_string(),
                    ));
                }
            }
        };

        if let Some(result_str) = result_str {
            if let Some(t) = token_to_remove
                && let Ok(mut map) = invocation_scope_by_token.lock()
            {
                map.remove(t);
            }
            {
                let mut guard = eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(eval_token);
            }
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
        if should_recheck {
            let new_timeout = poller.timeout_attempts().await;
            timeout_attempts = Some(timeout_attempts.map_or(new_timeout, |t| t.max(new_timeout)));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        attempts += 1;
        let limit = timeout_attempts.unwrap_or(u32::MAX);
        if attempts >= limit {
            if let Some(t) = token_to_remove
                && let Ok(mut map) = invocation_scope_by_token.lock()
            {
                map.remove(t);
            }
            {
                let mut guard = eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(eval_token);
            }
            return Err(BamlRtError::QuickJs(format!(
                "Promise did not resolve after {} attempts ({}ms)",
                limit, limit
            )));
        }
    }
}
