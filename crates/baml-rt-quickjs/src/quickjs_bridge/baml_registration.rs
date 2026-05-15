//! BAML and await/stream registration with QuickJS.
//!
//! All __baml_invoke, __baml_stream, __set_eval_result, await helpers, and
//! batched per-function wrapper registration live here so the main bridge focuses on coordination.
//!
//! ## Execution Session Invariants
//!
//! The `ExecutionSession` typestate FSM is the single source of truth for
//! intent/plan/step lifecycle within a task. It is shared (via `Arc<StdMutex>`)
//! with the tool execution layer so `resolve_planning_step` can read
//! `current_step_id` to attribute tool/LLM calls to plan steps.
//!
//! 1. **Typestate lifecycle** (compile-time):
//!    `AwaitIntent → AwaitPlan → Executable → Closed`
//!    Invalid transitions return `Err`; the enum prevents skipping phases.
//!
//! 2. **Scope immutability**:
//!    ∀ commands on session S: `scope == S.base.owner_scope`
//!    Enforced by `ensure_execution_session_scope_matches` on every command.
//!
//! 3. **Step coordinate availability**:
//!    ∀ tool/LLM calls during an in-progress step:
//!    `resolve_planning_step()` returns `Some((plan_id, step_id))`.
//!    Enforced by `startStep` setting `current_step_id`; `completeStep` clearing it.
//!
//! 4. **Known step membership**:
//!    ∀ `startStep`/`completeStep(step_id)`: `step_id ∈ step_status.keys()`
//!    Enforced by `apply_start_step` / `apply_complete_step` returning `Err`.
//!
//! 5. **Dependency ordering**:
//!    ∀ `startStep(step_id)`: all `depends_on(step_id) ⊆ completed`
//!    Enforced by `apply_start_step` checking `completed` set.
//!
//! 6. **Epoch monotonicity**:
//!    Supersessions increment epoch; non-supersessions preserve it.
//!    Enforced by `ensure_lineage_epoch_matches`.
//!
//! 7. **Current step exclusivity** (sequential execution):
//!    `current_step_id` tracks the most recent `startStep` call within a task.
//!    Concurrent step execution requires coroutine-local attribution (future work).

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
};

use baml_rt_core::{
    BamlRtError, Citation, Result,
    bus::PlanningSupersessionKind,
    context::{self, InvocationScope},
    correlation,
    ids::ExecutionSessionId,
};
use quickjs_runtime::{
    jsutils::Script, quickjsrealmadapter::QuickJsRealmAdapter, values::JsValueFacade,
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    QuickJSBridge,
    scope::{InvocationToken, resolve_scope_from_active_context, resolve_scope_from_session},
    stream_yield, tools,
    types::InFlightGuard,
    wrappers,
};
use crate::{
    execution_session_types::ExecutionSessionCommand, js_value_converter::value_to_js_value_facade,
    planning::IntentSubmission as PlanningIntentSubmission,
};

/// Base scope owned by every execution session variant.
#[derive(Debug, Clone)]
pub(crate) struct SessionBase {
    pub(crate) owner_scope: context::RuntimeScope,
    pub(crate) owner_task_id: String,
    pub(crate) owner_context_id: String,
}

/// Lineage epoch: incremented on supersession to reject stale step mutations.
type LineageEpoch = u64;

/// Step-transition event emitted by `apply_start_step` / `apply_complete_step`.
struct StepTransitionEvent {
    intent_id: String,
    plan_id: String,
    scope: context::RuntimeScope,
    step_id: String,
    old_status: String,
    new_status: String,
    citations: Vec<Citation>,
    epoch: LineageEpoch,
}

/// Abort event emitted per non-terminal step by `apply_abort`.
struct StepAbortEvent {
    intent_id: String,
    plan_id: String,
    scope: context::RuntimeScope,
    step_id: String,
    old_status: String,
    #[allow(dead_code)]
    reason: String,
    epoch: LineageEpoch,
}

/// Typestate: invalid transitions are unrepresentable.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Executable is large by design; all fields are needed for step lifecycle
pub(crate) enum ExecutionSession {
    AwaitIntent(SessionBase),
    AwaitPlan {
        base: SessionBase,
        intent_id: String,
        epoch: LineageEpoch,
    },
    Executable {
        base: SessionBase,
        intent_id: String,
        plan_id: String,
        plan_steps: Vec<String>,
        step_status: HashMap<String, String>,
        step_deps: HashMap<String, Vec<String>>,
        completed: HashSet<String>,
        epoch: LineageEpoch,
        current_step_id: Option<String>,
    },
    Closed(SessionBase),
}

impl ExecutionSession {
    fn base(&self) -> &SessionBase {
        match self {
            Self::AwaitIntent(b) => b,
            Self::AwaitPlan { base, .. } => base,
            Self::Executable { base, .. } => base,
            Self::Closed(b) => b,
        }
    }

    fn epoch(&self) -> Option<LineageEpoch> {
        match self {
            Self::AwaitPlan { epoch, .. } | Self::Executable { epoch, .. } => Some(*epoch),
            _ => None,
        }
    }

    fn new(owner_scope: context::RuntimeScope, owner_task_id: String) -> Self {
        let base = SessionBase {
            owner_context_id: owner_scope.context_id().as_str().to_string(),
            owner_scope,
            owner_task_id,
        };
        Self::AwaitIntent(base)
    }

    fn into_await_plan(
        self,
        intent_id: String,
        epoch: LineageEpoch,
    ) -> std::result::Result<Self, quickjs_runtime::jsutils::JsError> {
        match self {
            Self::AwaitIntent(base) => Ok(Self::AwaitPlan {
                base,
                intent_id,
                epoch,
            }),
            _ => Err(quickjs_runtime::jsutils::JsError::new_str(
                "execution session cannot submitIntent in current phase",
            )),
        }
    }

    fn into_executable(
        self,
        plan_id: String,
        plan_steps: Vec<String>,
        step_status: HashMap<String, String>,
        step_deps: HashMap<String, Vec<String>>,
        epoch: LineageEpoch,
    ) -> std::result::Result<Self, quickjs_runtime::jsutils::JsError> {
        match self {
            Self::AwaitPlan {
                base,
                intent_id,
                epoch: _,
            } => Ok(Self::Executable {
                base,
                intent_id,
                plan_id,
                plan_steps,
                step_status,
                step_deps,
                completed: HashSet::new(),
                epoch,
                current_step_id: None,
            }),
            _ => Err(quickjs_runtime::jsutils::JsError::new_str(
                "execution session cannot submitPlan in current phase",
            )),
        }
    }

    fn into_closed(self) -> std::result::Result<Self, quickjs_runtime::jsutils::JsError> {
        match self {
            Self::Executable { base, .. }
            | Self::AwaitPlan { base, .. }
            | Self::AwaitIntent(base) => Ok(Self::Closed(base)),
            Self::Closed(_) => Err(quickjs_runtime::jsutils::JsError::new_str(
                "execution session already closed",
            )),
        }
    }

    /// INVARIANT 3: Sets current_step_id for step coordinate availability.
    /// INVARIANT 4: Rejects unknown step_ids (known step membership).
    /// INVARIANT 5: Rejects if dependencies not completed (dependency ordering).
    fn apply_start_step(
        self,
        step_id: &str,
        citations: Vec<Citation>,
    ) -> std::result::Result<(Self, StepTransitionEvent), quickjs_runtime::jsutils::JsError> {
        let Self::Executable {
            base,
            intent_id,
            plan_id,
            plan_steps,
            mut step_status,
            step_deps,
            completed,
            epoch,
            current_step_id: _,
        } = self
        else {
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "execution session is not executable",
            ));
        };
        if !step_status.contains_key(step_id) {
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "stepId does not exist in plan",
            ));
        }
        let old_status = step_status
            .get(step_id)
            .cloned()
            .unwrap_or_else(|| "pending".to_string());
        if old_status != "pending" {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "step cannot transition to in_progress from {old_status}: {step_id}"
            )));
        }
        let deps = step_deps.get(step_id).cloned().unwrap_or_default();
        for dep in &deps {
            if !completed.contains(dep) {
                return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "cannot start step before dependency: {dep}"
                )));
            }
        }
        step_status.insert(step_id.to_string(), "in_progress".to_string());
        let new_session = Self::Executable {
            base: base.clone(),
            intent_id: intent_id.clone(),
            plan_id: plan_id.clone(),
            plan_steps,
            step_status,
            step_deps,
            completed,
            epoch,
            current_step_id: Some(step_id.to_string()),
        };
        let event = StepTransitionEvent {
            intent_id,
            plan_id,
            scope: base.owner_scope.clone(),
            step_id: step_id.to_string(),
            old_status,
            new_status: "in_progress".to_string(),
            citations,
            epoch,
        };
        Ok((new_session, event))
    }

    /// INVARIANT 3: Clears current_step_id when the completed step matches.
    /// INVARIANT 4: Rejects unknown step_ids.
    fn apply_complete_step(
        self,
        step_id: &str,
        citations: Vec<Citation>,
    ) -> std::result::Result<(Self, StepTransitionEvent), quickjs_runtime::jsutils::JsError> {
        let Self::Executable {
            base,
            intent_id,
            plan_id,
            plan_steps,
            mut step_status,
            step_deps,
            mut completed,
            epoch,
            current_step_id,
        } = self
        else {
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "execution session is not executable",
            ));
        };
        if !step_status.contains_key(step_id) {
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "stepId does not exist in plan",
            ));
        }
        let old_status = step_status
            .get(step_id)
            .cloned()
            .unwrap_or_else(|| "pending".to_string());
        if old_status != "in_progress" {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "step cannot transition to completed from {old_status}: {step_id}"
            )));
        }
        step_status.insert(step_id.to_string(), "completed".to_string());
        completed.insert(step_id.to_string());
        let new_current_step_id = if current_step_id.as_deref() == Some(step_id) {
            None
        } else {
            current_step_id
        };
        let new_session = Self::Executable {
            base: base.clone(),
            intent_id: intent_id.clone(),
            plan_id: plan_id.clone(),
            plan_steps,
            step_status,
            step_deps,
            completed,
            epoch,
            current_step_id: new_current_step_id,
        };
        let event = StepTransitionEvent {
            intent_id,
            plan_id,
            scope: base.owner_scope.clone(),
            step_id: step_id.to_string(),
            old_status,
            new_status: "completed".to_string(),
            citations,
            epoch,
        };
        Ok((new_session, event))
    }

    /// Returns (new_session, abort_events). Abort events: (intent_id, plan_id, scope, step_id, old_status, reason, epoch).
    fn apply_abort(self, reason: &str) -> (Self, Vec<StepAbortEvent>) {
        let (base, abort_events) = match self {
            Self::Executable {
                base,
                intent_id,
                plan_id,
                plan_steps,
                mut step_status,
                step_deps: _,
                completed: _,
                epoch,
                current_step_id: _,
            } => {
                let mut events = Vec::new();
                for step_id in &plan_steps {
                    let current_status = step_status
                        .get(step_id)
                        .cloned()
                        .unwrap_or_else(|| "pending".to_string());
                    if current_status == "completed" || current_status == "aborted" {
                        continue;
                    }
                    step_status.insert(step_id.clone(), "aborted".to_string());
                    events.push(StepAbortEvent {
                        intent_id: intent_id.clone(),
                        plan_id: plan_id.clone(),
                        scope: base.owner_scope.clone(),
                        step_id: step_id.clone(),
                        old_status: current_status,
                        reason: reason.to_string(),
                        epoch,
                    });
                }
                (base, events)
            }
            Self::AwaitIntent(base) | Self::AwaitPlan { base, .. } | Self::Closed(base) => {
                (base, Vec::new())
            }
        };
        (Self::Closed(base), abort_events)
    }
}

static PLANNING_SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_execution_session_id() -> ExecutionSessionId {
    let n = PLANNING_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    ExecutionSessionId::new(format!("execution:{}:{n}", uuid::Uuid::new_v4()))
}

fn parse_supersession(s: Option<&str>) -> Option<PlanningSupersessionKind> {
    match s.map(str::trim) {
        Some("replaced") | Some("replaced_by") | Some("replacedBy") => {
            Some(PlanningSupersessionKind::ReplacedBy)
        }
        Some("refined") | Some("refined_by") | Some("refinedBy") => {
            Some(PlanningSupersessionKind::RefinedBy)
        }
        _ => None,
    }
}

/// INVARIANT 6: Epoch monotonicity — session epoch must match the task's current epoch.
fn ensure_lineage_epoch_matches(
    session: &ExecutionSession,
    lineage_epoch: &HashMap<String, LineageEpoch>,
    task_id: &str,
) -> std::result::Result<(), quickjs_runtime::jsutils::JsError> {
    let Some(session_epoch) = session.epoch() else {
        return Ok(());
    };
    let current_epoch = lineage_epoch.get(task_id).copied().unwrap_or(0);
    if session_epoch != current_epoch {
        return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
            "stale lineage: session epoch {session_epoch} does not match current {current_epoch} (session was superseded)"
        )));
    }
    Ok(())
}

/// INVARIANT 2: Scope immutability — every command must match the session's owner scope.
fn ensure_execution_session_scope_matches(
    base: &SessionBase,
    scope: &context::RuntimeScope,
) -> std::result::Result<(), quickjs_runtime::jsutils::JsError> {
    let current_task = scope
        .task_id_opt()
        .map(|id| id.as_str().to_string())
        .ok_or_else(|| {
            quickjs_runtime::jsutils::JsError::new_str(
                "execution session action requires task scope",
            )
        })?;
    if current_task != base.owner_task_id
        || scope.context_id().as_str() != base.owner_context_id.as_str()
    {
        tracing::warn!(
            expected_task_id = %base.owner_task_id,
            actual_task_id = %current_task,
            expected_context_id = %base.owner_context_id,
            actual_context_id = %scope.context_id(),
            "Execution session scope mismatch"
        );
        return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
            "execution session scope mismatch: action rejected (expected task/context {}/{}, got {}/{})",
            base.owner_task_id,
            base.owner_context_id,
            current_task,
            scope.context_id().as_str()
        )));
    }
    Ok(())
}

fn is_archive_read_step_op(op: Option<&str>) -> bool {
    matches!(op, Some("SearchRead") | Some("PageRead"))
}

fn validate_step_executor_transition(
    session_open_before_hop: bool,
    last_status_before_hop: Option<&str>,
    op: Option<&str>,
    status: &str,
    step_executor: &str,
) -> std::result::Result<(), quickjs_runtime::jsutils::JsError> {
    if !session_open_before_hop {
        if status != "open" {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "runtime step executor contract violation ({step_executor}): expected Open-first hop to yield status 'open', got '{status}'"
            )));
        }
        return Ok(());
    }

    if last_status_before_hop == Some("open") {
        if is_archive_read_step_op(op) {
            if !(status == "streaming" || status == "suspended" || status == "done") {
                return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "runtime step executor contract violation ({step_executor}): expected SearchRead/PageRead hop status in [streaming,suspended,done], got '{status}'"
                )));
            }
            return Ok(());
        }
        if status != "sent" {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "runtime step executor contract violation ({step_executor}): expected Send-hop status 'sent', got '{status}'"
            )));
        }
        return Ok(());
    }

    if is_archive_read_step_op(op)
        && matches!(
            last_status_before_hop,
            Some("sent") | Some("streaming") | Some("suspended") | Some("done")
        )
    {
        if !(status == "streaming" || status == "suspended" || status == "done") {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "runtime step executor contract violation ({step_executor}): expected SearchRead/PageRead hop status in [streaming,suspended,done], got '{status}'"
            )));
        }
        return Ok(());
    }

    if op == Some("Send")
        && matches!(
            last_status_before_hop,
            Some("sent") | Some("streaming") | Some("suspended") | Some("done")
        )
    {
        if status != "sent" {
            return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
                "runtime step executor contract violation ({step_executor}): expected Send-hop status 'sent', got '{status}'"
            )));
        }
        return Ok(());
    }

    if last_status_before_hop == Some("done") && !(status == "done" || status == "finished") {
        return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
            "runtime step executor contract violation ({step_executor}): expected Finish-hop terminal status in [done,finished], got '{status}'"
        )));
    }

    Ok(())
}

/// Register __baml_invoke. Tokenless: host resolves scope from active context.
pub(super) async fn register_baml_invoke_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let registry = bridge.invocation_context_registry().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_invoke",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let scope = resolve_scope_from_active_context(&registry)?;
            if args.len() < 2 {
                return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (function_name, args)"));
            }

            let func_name_js = &args[0];
            let func_name = if func_name_js.is_string() {
                func_name_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
            };

            let args_js = &args[1];
            let args_json_str = if args_js.is_string() {
                args_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string - use JSON.stringify in JavaScript"));
            };

            let args_json: Value = serde_json::from_str(&args_json_str)
                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

            let func_name_clone = func_name.clone();
            let manager_for_promise = manager_clone.clone();
            let correlation_id = registry
                .lock()
                .ok()
                .and_then(|g| g.current_frame().ok())
                .and_then(|f| f.correlation_id);
            let scope_for_tools = scope.clone();

            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope, async move {
                        let manager = manager_for_promise.read().await;
                        let invocation_scope = InvocationScope::new(scope_for_tools.clone());
                        let value = manager
                            .invoke_function(&invocation_scope, &func_name_clone, args_json)
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        let result = manager
                            .execute_tool_from_baml_result_or_value(
                                &scope_for_tools,
                                value,
                                Some(&func_name_clone),
                                None,
                            )
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        Ok(value_to_js_value_facade(result))
                    })
                    .await
                };
                if let Some(correlation_id) = correlation_id {
                    correlation::with_correlation_id(correlation_id, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register helper function".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_invoke helper function with async promise support");
    Ok(())
}

/// Register __awaitAndStringify and __set_eval_result.
pub(super) async fn register_await_helper(bridge: &QuickJSBridge) -> Result<()> {
    let js_code = r#"
            globalThis.__awaitAndStringify = async function(promise) {
                try {
                    const result = await promise;
                    return JSON.stringify(result);
                } catch (e) {
                    return JSON.stringify({ error: e.toString() });
                }
            };

            globalThis.__isPromise = function(value) {
                return value && typeof value.then === 'function';
            };
        "#;

    let script = Script::new("await_helper.js", js_code);
    bridge
        .runtime()
        .eval(None, script)
        .await
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register await helper".to_string(),
            source: Box::new(e),
        })?;

    let eval_results = bridge.eval_results_by_token().clone();
    let eval_notify_by_token = bridge.eval_notify_by_token().clone();
    bridge.runtime().set_function(
        &[],
        "__set_eval_result",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            if args.len() < 2 {
                return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, json_string)"));
            }
            let token = if args[0].is_string() {
                args[0].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str("Token must be a string"));
            };
            let json_str = if args[1].is_string() {
                args[1].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str("json_string must be a string"));
            };
            let key = InvocationToken(token);
            if !eval_results.contains_key(&key) {
                // Late promise resolution after host cleanup (e.g. bounded resume poll timeout).
                // Ignore stale writes so we do not surface an unhandled rejection in JS.
                tracing::debug!(
                    token = %key.0,
                    "stale eval result: token slot already removed"
                );
                return Ok(JsValueFacade::Undefined);
            }
            eval_results.insert(key.clone(), Some(json_str));
            if let Some((_, notify)) = eval_notify_by_token.remove(&key) {
                tracing::debug!(token = %key.0, "eval result set, notifying poll loop");
                notify.notify_one();
            }
            Ok(JsValueFacade::Undefined)
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __set_eval_result".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __awaitAndStringify helper function");
    Ok(())
}

/// Register Step Executor runtime helpers so shim JS stays coordination-only.
pub(super) async fn register_step_executor_runtime_helpers(bridge: &QuickJSBridge) -> Result<()> {
    bridge
        .runtime()
        .set_function(
            &[],
            "__step_executor_validate_transition",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<
                JsValueFacade,
                quickjs_runtime::jsutils::JsError,
            > {
                if args.is_empty() || !args[0].is_string() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str(
                        "Expected step-executor transition JSON string",
                    ));
                }
                let payload: Value = serde_json::from_str(args[0].get_str()).map_err(|e| {
                    quickjs_runtime::jsutils::JsError::new_str(&format!(
                        "Failed to parse step-executor transition JSON: {e}"
                    ))
                })?;
                let session_open_before_hop = payload
                    .get("session_open_before_hop")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let last_status_before_hop =
                    payload.get("last_status_before_hop").and_then(Value::as_str);
                let op = payload.get("op").and_then(Value::as_str);
                let status = payload
                    .get("status")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        quickjs_runtime::jsutils::JsError::new_str(
                            "step-executor transition requires status",
                        )
                    })?;
                let step_executor = payload
                    .get("step_executor")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown_step_executor");
                validate_step_executor_transition(
                    session_open_before_hop,
                    last_status_before_hop,
                    op,
                    status,
                    step_executor,
                )?;
                Ok(JsValueFacade::Null)
            },
        )
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __step_executor_validate_transition helper".to_string(),
            source: Box::new(e),
        })?;

    // Resolve tool_name → single-tool step executor for polymorphic auto-narrowing.
    // Returns the executor function name (String) or null if not found.
    let manager_clone3 = bridge.baml_manager().clone();
    bridge
        .runtime()
        .set_function(
            &[],
            "__resolve_tool_step_executor",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<
                JsValueFacade,
                quickjs_runtime::jsutils::JsError,
            > {
                if args.is_empty() || !args[0].is_string() {
                    return Ok(JsValueFacade::Null);
                }
                let tool_name = args[0].get_str();
                let result = manager_clone3
                    .try_read()
                    .ok()
                    .and_then(|guard| guard.resolve_tool_step_executor(tool_name));
                match result {
                    Some(executor) => Ok(JsValueFacade::new_string(executor)),
                    None => Ok(JsValueFacade::Null),
                }
            },
        )
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __resolve_tool_step_executor helper".to_string(),
            source: Box::new(e),
        })?;

    // __run_step_executor: Rust-hosted step executor loop.
    // Takes (function_name, args_json, options_json?) from JS, runs the multi-hop
    // FSM loop entirely in Rust, returns [`StepExecutorOutcome`] as JSON (always resolves the promise).
    // Host supplement auto-retry (phase 2) remains disabled until metrics warrant it.
    // That payload is execution telemetry only; the canonical user reply is SessionResult.message.
    let manager_clone4 = bridge.baml_manager().clone();
    let registry_clone = bridge.invocation_context_registry().clone();
    bridge
        .runtime()
        .set_function(
            &[],
            "__run_step_executor",
            move |_realm: &QuickJsRealmAdapter,
                  args: Vec<JsValueFacade>|
                  -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 || !args[0].is_string() || !args[1].is_string() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str(
                        "__run_step_executor expects (function_name: string, args_json: string, options_json?: string)",
                    ));
                }
                let function_name = args[0].get_str().to_string();
                let args_json = args[1].get_str().to_string();
                let options_json = args
                    .get(2)
                    .filter(|v| v.is_string())
                    .map(|v| v.get_str().to_string());

                let scope =
                    crate::quickjs_bridge::scope::resolve_scope_from_active_context(
                        &registry_clone,
                    )?;

                let manager = manager_clone4.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(
                    async move {
                        let base_args: Value =
                            serde_json::from_str(&args_json).map_err(|e| {
                                quickjs_runtime::jsutils::JsError::new_str(&format!(
                                    "__run_step_executor: invalid args JSON: {e}"
                                ))
                            })?;

                        let max_steps = options_json
                            .as_deref()
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .and_then(|v| v.get("max_steps").and_then(Value::as_u64))
                            .map(|n| n as usize)
                            .unwrap_or(8);

                        let loop_result =
                            crate::step_executor_loop::run_step_executor_loop(
                                &manager,
                                &scope,
                                &function_name,
                                base_args,
                                max_steps,
                                None,
                            )
                            .await;

                        let outcome =
                            crate::step_executor_outcome_bridge::step_executor_outcome_from_loop_result(
                                loop_result,
                            );
                        let json = serde_json::to_string(&outcome).map_err(|e| {
                            quickjs_runtime::jsutils::JsError::new_str(&format!(
                                "__run_step_executor: serialize error: {e}"
                            ))
                        })?;
                        Ok(JsValueFacade::new_string(json))
                    },
                ))
            },
        )
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __run_step_executor helper".to_string(),
            source: Box::new(e),
        })?;

    tracing::debug!("Registered step-executor runtime helpers");
    Ok(())
}

/// Register `__execution_session_invoke(payload_json)` so execution-session state lives in Rust.
pub(super) async fn register_execution_session_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let registry = bridge.invocation_context_registry().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();
    let session_state = bridge.execution_sessions().clone();
    let lineage_epoch = Arc::new(StdMutex::new(HashMap::<String, LineageEpoch>::new()));

    bridge.runtime().set_function(
        &[],
        "__execution_session_invoke",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            if args.is_empty() || !args[0].is_string() {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected execution-session payload JSON string",
                ));
            }
            let payload: Value = serde_json::from_str(args[0].get_str()).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse execution-session payload JSON: {e}"
                ))
            })?;
            let cmd: ExecutionSessionCommand = serde_json::from_value(payload.clone()).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse execution-session command: {e}"
                ))
            })?;
            let session_id_for_scope = match &cmd {
                ExecutionSessionCommand::Open => None,
                ExecutionSessionCommand::SubmitIntent { session_id, .. }
                | ExecutionSessionCommand::SubmitPlan { session_id, .. }
                | ExecutionSessionCommand::StartStep { session_id, .. }
                | ExecutionSessionCommand::CompleteStep { session_id, .. }
                | ExecutionSessionCommand::Finish { session_id }
                | ExecutionSessionCommand::Abort { session_id, .. } => Some(session_id.clone()),
            };
            let invocation_scope = match &cmd {
                ExecutionSessionCommand::Open => resolve_scope_from_active_context(&registry)?,
                _ => {
                    let session_id = session_id_for_scope.as_ref().ok_or_else(|| {
                        quickjs_runtime::jsutils::JsError::new_str(
                            "execution-session payload requires non-empty session_id",
                        )
                    })?;
                    let session_ref = session_state.get(session_id.as_str()).ok_or_else(|| {
                        quickjs_runtime::jsutils::JsError::new_str("execution session not found")
                    })?;
                    session_ref.base().owner_scope.clone()
                }
            };
            let cmd_for_run = cmd.clone();

            let manager_for_promise = manager_clone.clone();
            let scope_for_run = invocation_scope.clone();
            let correlation_id = registry
                .lock()
                .ok()
                .and_then(|g| g.current_frame().ok())
                .and_then(|f| f.correlation_id);
            let state_store = session_state.clone();
            let lineage_epoch_store = lineage_epoch.clone();

            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(invocation_scope, async move {
                        match &cmd_for_run {
                            ExecutionSessionCommand::Open => {
                                let task_id = scope_for_run.task_id_opt().ok_or_else(|| {
                                    quickjs_runtime::jsutils::JsError::new_str(
                                        "execution session open requires task scope",
                                    )
                                })?;
                                let session_id = next_execution_session_id();
                                state_store.insert(
                                    session_id.as_str().to_string(),
                                    ExecutionSession::new(
                                        scope_for_run.clone(),
                                        task_id.as_str().to_string(),
                                    ),
                                );
                                let out = serde_json::json!({
                                    "sessionId": session_id.as_str(),
                                    "phase": "await_intent"
                                });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session open response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                            ExecutionSessionCommand::SubmitIntent { session_id, intent } => {
                                let supersession = match intent.supersession.as_deref() {
                                    Some(s) => Some(
                                        parse_supersession(Some(s)).ok_or_else(|| {
                                            quickjs_runtime::jsutils::JsError::new_str(&format!(
                                                "intent.supersession must be replaced|refined, got {s}"
                                            ))
                                        })?,
                                    ),
                                    None => None,
                                };

                                let task_id = scope_for_run.task_id_opt().ok_or_else(|| {
                                    quickjs_runtime::jsutils::JsError::new_str(
                                        "execution session submitIntent requires task scope",
                                    )
                                })?;
                                let task_id_str = task_id.as_str().to_string();

                                let (emit_scope, epoch) = {
                                    let mut epoch_guard = lineage_epoch_store.lock().map_err(|_| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "lineage epoch lock poisoned",
                                        )
                                    })?;
                                    let epoch = if supersession.is_some() {
                                        let next = epoch_guard
                                            .get(&task_id_str)
                                            .copied()
                                            .unwrap_or(0)
                                            .saturating_add(1);
                                        epoch_guard.insert(task_id_str.clone(), next);
                                        next
                                    } else {
                                        *epoch_guard.entry(task_id_str.clone()).or_insert(0)
                                    };
                                    drop(epoch_guard);

                                    let (_, session) = state_store.remove(session_id.as_str()).ok_or_else(|| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session not found",
                                        )
                                    })?;
                                    ensure_execution_session_scope_matches(
                                        session.base(),
                                        &scope_for_run,
                                    )?;
                                    let session = session.into_await_plan(
                                        intent.intent_id.as_str().to_string(),
                                        epoch,
                                    )?;
                                    let scope = session.base().owner_scope.clone();
                                    state_store.insert(session_id.as_str().to_string(), session);
                                    (scope, epoch)
                                };

                                // Message UUID lineage is host-only: agent / JS is adversarial and must not
                                // supply `derivedFromMessageIds` (ignored on wire if present).
                                let derived_from_message_ids =
                                    vec![emit_scope.message_id().as_str().to_string()];
                                let planning_submission = PlanningIntentSubmission {
                                    intent_id: intent.intent_id.clone(),
                                    description: intent.description.clone(),
                                    citations: intent.citations.clone(),
                                    derived_from_message_ids,
                                    supersession,
                                };

                                let manager = manager_for_promise.read().await;
                                manager
                                    .emit_planning_intent_resolved(
                                        &emit_scope,
                                        planning_submission,
                                        Some(epoch),
                                    )
                                    .await
                                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;

                                let out = serde_json::json!({
                                    "sessionId": session_id.as_str(),
                                    "phase": "await_plan"
                                });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session submit_intent response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                            ExecutionSessionCommand::SubmitPlan { session_id, plan } => {
                                let supersession = match plan.supersession.as_deref() {
                                    Some(s) => Some(
                                        parse_supersession(Some(s)).ok_or_else(|| {
                                            quickjs_runtime::jsutils::JsError::new_str(&format!(
                                                "plan.supersession must be replaced|refined, got {s}"
                                            ))
                                        })?,
                                    ),
                                    None => None,
                                };

                                let task_id = scope_for_run.task_id_opt().ok_or_else(|| {
                                    quickjs_runtime::jsutils::JsError::new_str(
                                        "execution session submitPlan requires task scope",
                                    )
                                })?;
                                let task_id_str = task_id.as_str().to_string();

                                let mut plan_steps_emit = Vec::with_capacity(plan.steps.len());
                                let mut plan_steps_ids = Vec::with_capacity(plan.steps.len());
                                let mut step_status = HashMap::new();
                                let mut step_deps = HashMap::new();
                                let mut seen = HashSet::new();

                                // Wire step_id / plan_id / intent_id are planning aliases (often LLM-authored).
                                // Canonical provenance entity ids are derived server-side from task_id + these strings.
                                for step in &plan.steps {
                                    let step_id = step.step_id.as_str().to_string();
                                    if !seen.insert(step_id.clone()) {
                                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                                            &format!("duplicate plan stepId: {step_id}"),
                                        ));
                                    }
                                    let deps: Vec<String> = step
                                        .depends_on
                                        .iter()
                                        .filter_map(|v| {
                                            let s = v.trim();
                                            if s.is_empty() { None } else { Some(s.to_string()) }
                                        })
                                        .collect();
                                    plan_steps_emit.push(serde_json::json!({
                                        "step_id": step_id,
                                        "description": step.description,
                                        "order": step.order,
                                        "depends_on": deps,
                                    }));
                                    plan_steps_ids.push(step_id.clone());
                                    step_status.insert(step_id.clone(), "pending".to_string());
                                    step_deps.insert(step_id, deps);
                                }

                                let (emit_scope, epoch) = {
                                    let mut epoch_guard = lineage_epoch_store.lock().map_err(|_| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "lineage epoch lock poisoned",
                                        )
                                    })?;
                                    let epoch = if supersession.is_some() {
                                        let next = epoch_guard
                                            .get(&task_id_str)
                                            .copied()
                                            .unwrap_or(0)
                                            .saturating_add(1);
                                        epoch_guard.insert(task_id_str.clone(), next);
                                        next
                                    } else {
                                        *epoch_guard.entry(task_id_str.clone()).or_insert(0)
                                    };
                                    drop(epoch_guard);

                                    let (_, session) = state_store.remove(session_id.as_str()).ok_or_else(|| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session not found",
                                        )
                                    })?;
                                    ensure_execution_session_scope_matches(
                                        session.base(),
                                        &scope_for_run,
                                    )?;
                                    if let ExecutionSession::AwaitPlan {
                                        intent_id: session_intent_id,
                                        ..
                                    } = &session
                                    {
                                        if session_intent_id != plan.intent_id.as_str() {
                                            state_store.insert(session_id.as_str().to_string(), session);
                                            return Err(quickjs_runtime::jsutils::JsError::new_str(
                                                "plan.intentId must match submitted intentId",
                                            ));
                                        }
                                    } else {
                                        state_store.insert(session_id.as_str().to_string(), session);
                                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session cannot submitPlan in current phase",
                                        ));
                                    }
                                    let session = session.into_executable(
                                        plan.plan_id.as_str().to_string(),
                                        plan_steps_ids,
                                        step_status,
                                        step_deps,
                                        epoch,
                                    )?;
                                    let scope = session.base().owner_scope.clone();
                                    state_store.insert(session_id.as_str().to_string(), session);
                                    (scope, epoch)
                                };

                                let manager = manager_for_promise.read().await;
                                manager
                                    .emit_planning_plan_generated(
                                        &emit_scope,
                                        plan.intent_id.as_str().to_string(),
                                        plan.plan_id.as_str().to_string(),
                                        Value::Array(plan_steps_emit),
                                        supersession,
                                        Some(epoch),
                                    )
                                    .await
                                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;

                                let out = serde_json::json!({
                                    "sessionId": session_id.as_str(),
                                    "phase": "executable"
                                });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session submit_plan response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                            ExecutionSessionCommand::StartStep {
                                session_id,
                                step_id,
                                citations,
                            }
                            | ExecutionSessionCommand::CompleteStep {
                                session_id,
                                step_id,
                                citations,
                            } => {
                                let is_start = matches!(&cmd_for_run, ExecutionSessionCommand::StartStep { .. });
                                let step_id_str = step_id.as_str().to_string();
                                let event = {
                                    let (_, session) = state_store.remove(session_id.as_str()).ok_or_else(|| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session not found",
                                        )
                                    })?;
                                    ensure_execution_session_scope_matches(
                                        session.base(),
                                        &scope_for_run,
                                    )?;
                                    let epoch_guard = lineage_epoch_store.lock().map_err(|_| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "lineage epoch lock poisoned",
                                        )
                                    })?;
                                    ensure_lineage_epoch_matches(
                                        &session,
                                        &epoch_guard,
                                        &session.base().owner_task_id,
                                    )?;
                                    drop(epoch_guard);
                                    let (session, event) = if is_start {
                                        session.apply_start_step(&step_id_str, citations.to_vec())?
                                    } else {
                                        session.apply_complete_step(&step_id_str, citations.to_vec())?
                                    };
                                    state_store.insert(session_id.as_str().to_string(), session);
                                    event
                                };

                                let manager = manager_for_promise.read().await;
                                manager
                                    .emit_planning_step_status_changed(
                                        &event.scope,
                                        event.intent_id,
                                        event.plan_id,
                                        event.step_id,
                                        Some(event.old_status),
                                        event.new_status,
                                        event.citations,
                                        Some(event.epoch),
                                    )
                                    .await
                                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                                let out = serde_json::json!({ "sessionId": session_id.as_str(), "ok": true });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session step response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                            ExecutionSessionCommand::Finish { session_id } => {
                                {
                                    // Validate with read-only access first, then remove+mutate.
                                    {
                                        let session_ref = state_store.get(session_id.as_str()).ok_or_else(|| {
                                            quickjs_runtime::jsutils::JsError::new_str(
                                                "execution session not found",
                                            )
                                        })?;
                                        ensure_execution_session_scope_matches(
                                            session_ref.base(),
                                            &scope_for_run,
                                        )?;
                                        let epoch_guard = lineage_epoch_store.lock().map_err(|_| {
                                            quickjs_runtime::jsutils::JsError::new_str(
                                                "lineage epoch lock poisoned",
                                            )
                                        })?;
                                        ensure_lineage_epoch_matches(
                                            session_ref.value(),
                                            &epoch_guard,
                                            &session_ref.base().owner_task_id,
                                        )?;
                                        drop(epoch_guard);
                                        if matches!(session_ref.value(), ExecutionSession::Closed(_)) {
                                            let out = serde_json::json!({ "sessionId": session_id.as_str(), "closed": true });
                                            return Ok(JsValueFacade::new_string(
                                                serde_json::to_string(&out).map_err(|e| {
                                                    quickjs_runtime::jsutils::JsError::new_str(&format!(
                                                        "Failed to encode execution-session finish response: {e}"
                                                    ))
                                                })?,
                                            ));
                                        }
                                    }
                                    let (_, session) = state_store.remove(session_id.as_str()).unwrap();
                                    if !matches!(session, ExecutionSession::Executable { .. }) {
                                        state_store.insert(session_id.as_str().to_string(), session);
                                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session cannot finish in current phase",
                                        ));
                                    }
                                    if let ExecutionSession::Executable {
                                        plan_steps,
                                        completed,
                                        ..
                                    } = &session
                                    {
                                        for step_id in plan_steps {
                                            if !completed.contains(step_id) {
                                                state_store.insert(session_id.as_str().to_string(), session);
                                                return Err(quickjs_runtime::jsutils::JsError::new_str(
                                                    "cannot finish before all steps are completed",
                                                ));
                                            }
                                        }
                                    }
                                    let session_clone = session.clone();
                                    let session = session
                                        .into_closed()
                                        .inspect_err(|_| {
                                            state_store.insert(session_id.as_str().to_string(), session_clone);
                                        })?;
                                    state_store.insert(session_id.as_str().to_string(), session);
                                }
                                let out = serde_json::json!({ "sessionId": session_id.as_str(), "closed": true });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session finish response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                            ExecutionSessionCommand::Abort { session_id, reason } => {
                                let reason = if reason.trim().is_empty() {
                                    "execution session aborted".to_string()
                                } else {
                                    reason.trim().to_string()
                                };
                                let abort_events = {
                                    let (_, session) = state_store.remove(session_id.as_str()).ok_or_else(|| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "execution session not found",
                                        )
                                    })?;
                                    ensure_execution_session_scope_matches(
                                        session.base(),
                                        &scope_for_run,
                                    )?;
                                    let epoch_guard = lineage_epoch_store.lock().map_err(|_| {
                                        quickjs_runtime::jsutils::JsError::new_str(
                                            "lineage epoch lock poisoned",
                                        )
                                    })?;
                                    ensure_lineage_epoch_matches(
                                        &session,
                                        &epoch_guard,
                                        &session.base().owner_task_id,
                                    )?;
                                    drop(epoch_guard);
                                    let (session, abort_events) = session.apply_abort(&reason);
                                    state_store.insert(session_id.as_str().to_string(), session);
                                    abort_events
                                };

                                if !abort_events.is_empty() {
                                    let manager = manager_for_promise.read().await;
                                    for evt in abort_events {
                                        manager
                                            .emit_planning_step_status_changed(
                                                &evt.scope,
                                                evt.intent_id,
                                                evt.plan_id,
                                                evt.step_id,
                                                Some(evt.old_status),
                                                "aborted".to_string(),
                                                vec![],
                                                Some(evt.epoch),
                                            )
                                            .await
                                            .map_err(|e| {
                                                quickjs_runtime::jsutils::JsError::new_str(&e.to_string())
                                            })?;
                                    }
                                }

                                let out = serde_json::json!({ "sessionId": session_id.as_str(), "closed": true });
                                Ok(JsValueFacade::new_string(
                                    serde_json::to_string(&out).map_err(|e| {
                                        quickjs_runtime::jsutils::JsError::new_str(&format!(
                                            "Failed to encode execution-session abort response: {e}"
                                        ))
                                    })?,
                                ))
                            }
                        }
                    })
                    .await
                };
                if let Some(correlation_id) = correlation_id {
                    correlation::with_correlation_id(correlation_id, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __execution_session_invoke helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __execution_session_invoke helper");
    Ok(())
}

/// Register all `globalThis[fn]` → `__baml_invoke` wrappers in one eval.
pub(super) async fn register_baml_invoke_wrappers_batch(
    bridge: &QuickJSBridge,
    function_names: &[String],
) -> Result<()> {
    if function_names.is_empty() {
        return Ok(());
    }

    let js_code = wrappers::build_baml_invoke_wrappers_batch(function_names);
    let script = Script::new("register_functions_batch.js", &js_code);
    bridge
        .runtime()
        .eval(None, script)
        .await
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register BAML functions (batch)".to_string(),
            source: Box::new(e),
        })?;

    tracing::debug!(
        function_count = function_names.len(),
        "Registered BAML invoke wrappers with QuickJS (batch eval)"
    );
    Ok(())
}

/// Register all `globalThis[fnStream]` → `__baml_stream` wrappers in one eval.
pub(super) async fn register_baml_stream_wrappers_batch(
    bridge: &QuickJSBridge,
    function_names: &[String],
) -> Result<()> {
    if function_names.is_empty() {
        return Ok(());
    }

    let js_code = wrappers::build_baml_stream_wrappers_batch(function_names);
    let script = Script::new("register_stream_functions_batch.js", &js_code);
    bridge
        .runtime()
        .eval(None, script)
        .await
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register BAML stream functions (batch)".to_string(),
            source: Box::new(e),
        })?;

    tracing::debug!(
        function_count = function_names.len(),
        "Registered BAML stream wrappers with QuickJS (batch eval)"
    );
    Ok(())
}

/// Register __baml_stream. Tokenless: host resolves scope from active context. JS calls (function_name, args).
pub(super) async fn register_baml_stream_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let registry = bridge.invocation_context_registry().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();
    let stream_sessions = bridge.stream_sessions().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_stream",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let scope = resolve_scope_from_active_context(&registry)?;
            if args.len() < 2 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (function_name, args)",
                ));
            }

            let func_name_js = &args[0];
            let func_name = if func_name_js.is_string() {
                func_name_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };

            let args_js = &args[1];
            let args_json_str = if args_js.is_string() {
                args_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string - use JSON.stringify in JavaScript",
                ));
            };

            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let (context_tags, stream_session_id_for_chunks) = stream_sessions
                .iter()
                .find(|entry| !entry.value().is_terminated() && entry.value().scope == scope)
                .map(|entry| (entry.value().context_tags.clone(), Some(entry.key().0)))
                .unwrap_or((None, None));

            let func_name_clone = func_name.clone();
            let manager_for_promise = manager_clone.clone();
            let scope_for_scope = scope.clone();
            let correlation_id = registry
                .lock()
                .ok()
                .and_then(|g| g.current_frame().ok())
                .and_then(|f| f.correlation_id);

            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope_for_scope.clone(), async move {
                        let inv = {
                            let manager = manager_for_promise.read().await;
                            manager.invoke_function_stream(
                                &scope_for_scope,
                                &func_name_clone,
                                args_json.clone(),
                                context_tags,
                            )
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                        };

                        let (tx, mut rx) = mpsc::channel::<Value>(64);
                        let tx_closure = tx.clone();

                        let crate::baml_execution::BamlStreamInvocation {
                            mut stream,
                            ctx_manager,
                            client_registry_opt,
                            env_vars,
                        } = inv;

                        stream_yield::scope_stream_yield(Some(tx), async move {
                            let (_result, _call_id) = stream
                                .run(
                                    None::<fn()>,
                                    Some(move |fr: baml_runtime::FunctionResult| {
                                        if let Some(Ok(parsed)) = fr.parsed().as_ref()
                                            && let Ok(mut v) =
                                                serde_json::to_value(parsed.serialize_partial())
                                        {
                                            if let Some(session_id) = stream_session_id_for_chunks
                                                && let Some(obj) = v.as_object_mut()
                                            {
                                                obj.insert(
                                                    "__session".to_string(),
                                                    serde_json::Value::from(session_id),
                                                );
                                            }
                                            if let Err(e) = tx_closure.try_send(v) {
                                                tracing::warn!(error = ?e, "Stream chunk dropped: channel full");
                                            }
                                        }
                                    }),
                                    &ctx_manager,
                                    None,
                                    client_registry_opt.as_ref(),
                                    env_vars,
                                )
                                .await;
                        })
                        .await;

                        let mut chunks = Vec::new();
                        while let Ok(v) = rx.try_recv() {
                            chunks.push(v);
                        }
                        while let Some(v) = rx.recv().await {
                            chunks.push(v);
                        }
                        Ok(value_to_js_value_facade(Value::Array(chunks)))
                    })
                    .await
                };
                if let Some(correlation_id) = correlation_id {
                    correlation::with_correlation_id(correlation_id, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __baml_stream helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_stream helper function");
    Ok(())
}

/// Register `__baml_invoke_session(session_id, function_name, args_json)`.
pub(super) async fn register_baml_invoke_session_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let stream_sessions = bridge.stream_sessions().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_invoke_session",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let session_id = tools::parse_session_id_arg(&args)?;
            let (scope, session) = match resolve_scope_from_session(&stream_sessions, session_id) {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = e.to_string();
                    return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                        Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                    }));
                }
            };
            if args.len() < 3 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (session_id, function_name, args_json)",
                ));
            }
            let func_name = if args[1].is_string() {
                args[1].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };
            let args_json_str = if args[2].is_string() {
                args[2].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string",
                ));
            };
            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let correlation_id = session.correlation_id.clone();
            let scope_for_tools = scope.clone();
            let manager_for_promise = manager_clone.clone();
            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope, async move {
                        let manager = manager_for_promise.read().await;
                        let invocation_scope = InvocationScope::new(scope_for_tools.clone());
                        let value = manager
                            .invoke_function(&invocation_scope, &func_name, args_json)
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        let result = manager
                            .execute_tool_from_baml_result_or_value(
                                &scope_for_tools,
                                value,
                                Some(&func_name),
                                None,
                            )
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        Ok(value_to_js_value_facade(result))
                    })
                    .await
                };
                if let Some(cid) = correlation_id {
                    correlation::with_correlation_id(cid, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __baml_invoke_session helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_invoke_session helper");
    Ok(())
}

/// Register `__baml_stream_session(session_id, function_name, args_json)`.
pub(super) async fn register_baml_stream_session_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let stream_sessions = bridge.stream_sessions().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_stream_session",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let session_id = tools::parse_session_id_arg(&args)?;
            let (scope, session) = match resolve_scope_from_session(&stream_sessions, session_id) {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = e.to_string();
                    return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                        Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                    }));
                }
            };
            if args.len() < 3 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (session_id, function_name, args_json)",
                ));
            }
            let func_name = if args[1].is_string() {
                args[1].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };
            let args_json_str = if args[2].is_string() {
                args[2].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string",
                ));
            };
            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let context_tags = session.context_tags.clone();
            let stream_session_id_for_chunks = session_id.0;
            let correlation_id = session.correlation_id.clone();
            let scope_for_run = scope.clone();
            let manager_for_promise = manager_clone.clone();
            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope, async move {
                        let inv = manager_for_promise
                            .read()
                            .await
                            .invoke_function_stream(
                                &scope_for_run,
                                &func_name,
                                args_json.clone(),
                                context_tags,
                            )
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;

                        let (tx, mut rx) = mpsc::channel::<Value>(64);
                        let tx_closure = tx.clone();

                        let crate::baml_execution::BamlStreamInvocation {
                            mut stream,
                            ctx_manager,
                            client_registry_opt,
                            env_vars,
                        } = inv;

                        stream_yield::scope_stream_yield(Some(tx), async move {
                            let (_result, _call_id) = stream
                                .run(
                                    None::<fn()>,
                                    Some(move |fr: baml_runtime::FunctionResult| {
                                        if let Some(Ok(parsed)) = fr.parsed().as_ref()
                                            && let Ok(mut v) =
                                                serde_json::to_value(parsed.serialize_partial())
                                        {
                                            if let Some(obj) = v.as_object_mut() {
                                                obj.insert(
                                                    "__session".to_string(),
                                                    serde_json::Value::from(stream_session_id_for_chunks),
                                                );
                                            }
                                            if let Err(e) = tx_closure.try_send(v) {
                                                tracing::warn!(error = ?e, "Stream chunk dropped: channel full");
                                            }
                                        }
                                    }),
                                    &ctx_manager,
                                    None,
                                    client_registry_opt.as_ref(),
                                    env_vars,
                                )
                                .await;
                        })
                        .await;

                        let mut chunks = Vec::new();
                        while let Ok(v) = rx.try_recv() {
                            chunks.push(v);
                        }
                        while let Some(v) = rx.recv().await {
                            chunks.push(v);
                        }
                        Ok(value_to_js_value_facade(Value::Array(chunks)))
                    })
                    .await
                };
                if let Some(cid) = correlation_id {
                    correlation::with_correlation_id(cid, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __baml_stream_session helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_stream_session helper");
    Ok(())
}
