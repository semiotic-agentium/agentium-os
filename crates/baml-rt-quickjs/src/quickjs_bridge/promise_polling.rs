//! Promise resolution polling for `evaluate()`.
//!
//! When JS returns a promise, the host runs pending jobs and checks
//! `__eval_result` in a loop until the promise resolves or timeout.
//! Effect-gated timeout (L5–L6) distinguishes "waiting on effect" from
//! "will never yield". See docs/HOST_QUICKJS_STREAM_INVARIANTS.md.

use crate::quickjs_bridge::eval::EffectGatedPoller;
use baml_rt_core::context::{InvocationScope, RuntimeScope};
use baml_rt_core::effects::EffectLiveness;
use baml_rt_core::{BamlRtError, Result};
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use super::scope::InvocationToken;

type EvalResultMap = Arc<StdMutex<HashMap<InvocationToken, Option<String>>>>;
type InvocationScopeMap = Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>;

const EFFECT_CHECK_INTERVAL: u32 = 100;

/// Poll until `__eval_result` is set for the given token or timeout.
///
/// Runs `runtime.run_pending_jobs_if_any()` each iteration so promise
/// continuations can run. Uses effect-gated timeout: long timeout when
/// effects are in-flight, short idle timeout otherwise. When the loop
/// exits (success or timeout), removes the invocation token from
/// `invocation_scope_by_token` if `token_to_remove` is `Some`.
pub(crate) async fn poll_promise_until_result(
    runtime: &QuickJsRuntimeFacade,
    eval_results_by_token: &EvalResultMap,
    eval_token: &InvocationToken,
    token_to_remove: Option<&InvocationToken>,
    invocation_scope_by_token: &InvocationScopeMap,
    scope: Option<&InvocationScope>,
    effect_liveness: Option<Arc<dyn EffectLiveness>>,
    idle_timeout_ms: u64,
    max_attempts_ms: u64,
) -> Result<String> {
    let context_id = scope
        .map(|s| s.context_id.clone())
        .or_else(baml_rt_core::context::current_context_id);
    let poller = EffectGatedPoller::new(
        effect_liveness,
        context_id,
        idle_timeout_ms,
        max_attempts_ms,
    );
    let mut timeout_attempts = poller.timeout_attempts().await;
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
            if let Some(t) = token_to_remove {
                if let Ok(mut map) = invocation_scope_by_token.lock() {
                    map.remove(t);
                }
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

        if attempts > 0 && attempts % EFFECT_CHECK_INTERVAL == 0 {
            let new_timeout = poller.timeout_attempts().await;
            timeout_attempts = timeout_attempts.max(new_timeout);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        attempts += 1;
        if attempts >= timeout_attempts {
            if let Some(t) = token_to_remove {
                if let Ok(mut map) = invocation_scope_by_token.lock() {
                    map.remove(t);
                }
            }
            {
                let mut guard = eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(eval_token);
            }
            return Err(BamlRtError::QuickJs(format!(
                "Promise did not resolve after {} attempts ({}ms)",
                timeout_attempts, timeout_attempts
            )));
        }
    }
}
