//! Typed step executor loop.
//!
//! Drives the multi-hop BAML function invocation loop entirely in Rust.
//! Replaces the JS `runGeneratedStepExecutor` shim — all FSM state, policy
//! resolution, polymorphic narrowing, and transition validation live here
//! with no invalid state representation.
//!
//! ## Provenance
//!
//! [`StepExecutorResult`] is **execution telemetry** (per-hop JSON, `last`, selected tool).
//! It must not be treated as the canonical user-visible chat reply: that role belongs to
//! `SessionResult.message` returned from the agent handler (e.g. structured synthesis).
//! Hosts record the surfaced reply from the chat completion path, not a duplicate scraped
//! from step envelopes.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{BamlFunctionId, BamlPromptName, BamlRtError, Result, VariantPhase, context};
use baml_rt_observability::metrics;
use baml_rt_tools::{ToolName, ToolSlug};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::baml::BamlRuntimeManager;

/// FSM status extracted from a tool session plan execution result.
/// Streaming/Suspended/Sent are now invisible to the LLM — Send blocks until Done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Open,
    Done,
    Finished,
    Aborted,
    /// Legacy statuses emitted during internal polling; mapped to Done at the FSM boundary.
    Sent,
    Streaming,
    Suspended,
}

impl StepStatus {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "finished" => Some(Self::Finished),
            "aborted" => Some(Self::Aborted),
            // Legacy / internal statuses — kept for graceful handling
            "sent" => Some(Self::Sent),
            "streaming" => Some(Self::Streaming),
            "suspended" => Some(Self::Suspended),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
            Self::Sent => "done",      // normalise legacy → done
            Self::Streaming => "done", // normalise legacy → done
            Self::Suspended => "done", // normalise legacy → done
        }
    }
}

/// Status values that are valid while a session is still active.
/// Only `JustOpened` (pre-Send) and `Done` (post-Send result) are visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenStatus {
    JustOpened,
    Done,
}

impl OpenStatus {
    fn try_from_step(s: StepStatus) -> Option<Self> {
        match s {
            StepStatus::Open => Some(Self::JustOpened),
            // All of Done/Sent/Streaming/Suspended normalise to Done from the FSM's perspective
            StepStatus::Done | StepStatus::Sent | StepStatus::Streaming | StepStatus::Suspended => {
                Some(Self::Done)
            }
            StepStatus::Finished | StepStatus::Aborted => None,
        }
    }
}

/// Why did the session terminate?
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum TerminalReason {
    Finished,
    Aborted,
    MaxStepsExhausted,
    MissingStatus,
}

/// Tool identity bound after the Open phase.
#[derive(Debug, Clone)]
struct ToolBinding {
    name: ToolName,
    slug: ToolSlug,
}

/// Step executor FSM phase. Impossible states are unrepresentable:
/// - `AwaitingOpen` has no tool identity (pre-selection)
/// - `Bound` always carries tool identity + a non-terminal status
/// - `Terminal` cannot transition further
#[derive(Debug)]
enum Phase {
    AwaitingOpen,
    Bound {
        tool: ToolBinding,
        status: OpenStatus,
    },
    #[allow(dead_code)]
    Terminal(TerminalReason),
}

impl Phase {
    fn is_session_open(&self) -> bool {
        matches!(self, Self::Bound { .. })
    }

    fn selected_tool(&self) -> Option<&ToolName> {
        match self {
            Self::Bound { tool, .. } => Some(&tool.name),
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn tool_slug(&self) -> Option<&ToolSlug> {
        match self {
            Self::Bound { tool, .. } => Some(&tool.slug),
            _ => None,
        }
    }
}

/// Build the `session_context` JSON injected into BAML function args.
///
/// FSM facts only — which operation is legal is expressed by the **per-phase**
/// BAML function's narrowed return type (`ExecuteStep__select`, `__act__`, …),
/// not by a redundant `allowed_ops` list in the prompt.
fn build_session_context(phase: &Phase) -> Value {
    serde_json::json!({
        "contract_version": "session_context",
        "session_open": phase.is_session_open(),
    })
}

/// Extract the status string from a tool execution result.
/// Handles `{ "status": "..." }` and `{ "output": { "status": "..." } }`.
fn extract_status(result: &Value) -> Option<StepStatus> {
    let s = result.get("status").and_then(Value::as_str).or_else(|| {
        result
            .get("output")
            .and_then(|o| o.get("status"))
            .and_then(Value::as_str)
    })?;
    StepStatus::parse(s)
}

/// Result of a completed step executor loop (FSM hops only — not the chat-layer user reply).
#[derive(Debug, serde::Serialize)]
pub struct StepExecutorResult {
    pub last: Value,
    pub steps: Vec<Value>,
    pub session_context: Value,
    pub selected_tool: Option<ToolName>,
}

/// Select the per-phase BAML function identity for polymorphic narrowing.
fn phase_function_id(base: &BamlPromptName, phase: &Phase) -> BamlFunctionId {
    match phase {
        Phase::AwaitingOpen => BamlFunctionId::variant(base.clone(), VariantPhase::Select),
        Phase::Bound {
            tool,
            status: OpenStatus::JustOpened,
        } => BamlFunctionId::variant(
            base.clone(),
            VariantPhase::Act {
                tool_slug: tool.slug.to_string(),
            },
        ),
        Phase::Bound {
            tool,
            status: OpenStatus::Done,
        } => BamlFunctionId::variant(
            base.clone(),
            VariantPhase::Continue {
                tool_slug: tool.slug.to_string(),
            },
        ),
        Phase::Terminal(_) => BamlFunctionId::base(base.as_str()),
    }
}

/// Extract tool_name from a step executor result (Open step output).
fn extract_tool_binding(result: &Value) -> Option<ToolBinding> {
    let raw = result
        .get("tool_name")
        .and_then(Value::as_str)
        .or_else(|| {
            result
                .get("step")
                .and_then(|s| s.get("tool_name"))
                .and_then(Value::as_str)
        })?;

    match ToolName::parse(raw) {
        Ok(name) => {
            let slug = name.slug();
            Some(ToolBinding { name, slug })
        }
        Err(e) => {
            tracing::warn!(
                raw_tool_name = raw,
                error = %e,
                "step_executor_loop: invalid tool_name in result, ignoring"
            );
            None
        }
    }
}

/// Run the step executor loop entirely in Rust.
///
/// Locks the manager per-hop (not for the entire loop) so other host helpers
/// (`__execution_session_invoke`, `__baml_invoke`) can interleave. Each hop:
/// lock -> invoke_function -> unlock -> advance FSM.
pub async fn run_step_executor_loop(
    manager: &Arc<RwLock<BamlRuntimeManager>>,
    scope: &context::RuntimeScope,
    function_name: &str,
    base_args: Value,
    max_steps: usize,
) -> Result<StepExecutorResult> {
    let base = BamlPromptName::new(function_name);
    let mut phase = Phase::AwaitingOpen;
    let mut steps: Vec<Value> = Vec::new();
    let mut last = Value::Null;

    let step_exec_loop_started_at = Instant::now();
    let mut step_exec_llm_hop_count_total: u64 = 0;
    let mut step_exec_llm_hop_count_select: u64 = 0;
    let mut step_exec_llm_hop_count_act: u64 = 0;
    let mut step_exec_llm_hop_count_continue: u64 = 0;
    let mut step_exec_llm_hop_latency_ms_total: u64 = 0;
    let mut step_exec_llm_hop_latency_ms_select: u64 = 0;
    let mut step_exec_llm_hop_latency_ms_act: u64 = 0;
    let mut step_exec_llm_hop_latency_ms_continue: u64 = 0;
    let mut step_exec_status_done_count: u64 = 0;
    let mut step_exec_status_finished_count: u64 = 0;
    let mut step_exec_status_aborted_count: u64 = 0;

    {
        let guard = manager.read().await;
        let p = guard.resolve_session_policy_for_function(function_name);
        tracing::info!(
            function = function_name,
            policy = ?p,
            "step_executor_loop: resolved session policy"
        );
    }

    for hop_idx in 0..max_steps {
        if matches!(phase, Phase::Terminal(_)) {
            break;
        }

        let function_id = phase_function_id(&base, &phase);
        let candidate = function_id.full_name();

        // Phase function MUST exist — absence means the agent package was built without
        // phase function generation (codegen bug or stale tarball). Fail hard so the
        // error surfaces immediately rather than silently using the full-union base
        // function, which would let the LLM emit any op regardless of FSM state.
        {
            let guard = manager.read().await;
            if guard.get_function_signature(&candidate).is_none() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "step executor: phase function '{candidate}' not found in agent schema. \
                     Rebuild the agent package to regenerate phase functions.",
                )));
            }
        }
        let current_function = candidate.clone();

        let session_context = build_session_context(&phase);

        let mut args = match base_args.as_object() {
            Some(obj) => Value::Object(obj.clone()),
            None => Value::Object(serde_json::Map::new()),
        };
        if let Some(obj) = args.as_object_mut() {
            obj.insert("session_context".to_string(), session_context);
        }

        let hop_phase_label = match &phase {
            Phase::AwaitingOpen => "select",
            Phase::Bound {
                status: OpenStatus::JustOpened,
                ..
            } => "act",
            Phase::Bound {
                status: OpenStatus::Done,
                ..
            } => "continue",
            Phase::Terminal(_) => "terminal",
        };

        tracing::info!(
            hop = hop_idx,
            prompt = %function_id.prompt_name(),
            function = %current_function,
            phase = ?phase,
            hop_phase = hop_phase_label,
            "step_executor_loop: starting hop"
        );

        let hop_started_at = Instant::now();
        let result = {
            let guard = manager.read().await;
            guard.invoke_function(scope, &current_function, args).await
        };
        let hop_latency_ms = hop_started_at.elapsed().as_millis() as u64;

        step_exec_llm_hop_count_total += 1;
        step_exec_llm_hop_latency_ms_total += hop_latency_ms;
        metrics::record_step_executor_hop(
            function_name,
            hop_phase_label,
            std::time::Duration::from_millis(hop_latency_ms),
        );
        match hop_phase_label {
            "select" => {
                step_exec_llm_hop_count_select += 1;
                step_exec_llm_hop_latency_ms_select += hop_latency_ms;
            }
            "act" => {
                step_exec_llm_hop_count_act += 1;
                step_exec_llm_hop_latency_ms_act += hop_latency_ms;
            }
            "continue" => {
                step_exec_llm_hop_count_continue += 1;
                step_exec_llm_hop_latency_ms_continue += hop_latency_ms;
            }
            _ => {}
        }

        match &result {
            Ok(v) => tracing::info!(hop = hop_idx, result = %v, "step_executor_loop: hop ok"),
            Err(e) => tracing::error!(hop = hop_idx, error = %e, "step_executor_loop: hop error"),
        }

        let result = result?;
        // Capture the last result with meaningful output (Done has output,
        // Finished/Open/Sent are status-only). This ensures `last` contains
        // the tool data, not a bare `{"status":"finished"}`.
        let has_content = result.get("output").is_some() || result.get("message").is_some();
        if has_content || last.is_null() {
            last = result.clone();
        }
        steps.push(result.clone());

        let Some(status) = extract_status(&result) else {
            phase = Phase::Terminal(TerminalReason::MissingStatus);
            break;
        };

        match status {
            StepStatus::Done | StepStatus::Sent | StepStatus::Streaming | StepStatus::Suspended => {
                step_exec_status_done_count += 1;
            }
            StepStatus::Finished => step_exec_status_finished_count += 1,
            StepStatus::Aborted => step_exec_status_aborted_count += 1,
            StepStatus::Open => {}
        }

        // Advance FSM based on current phase + status.
        phase = match phase {
            Phase::AwaitingOpen => {
                if status != StepStatus::Open {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "step executor contract violation ({current_function}): \
                         expected Open-first hop to yield status 'open', got '{}'",
                        status.as_str()
                    )));
                }
                let tool = extract_tool_binding(&result);
                match tool {
                    Some(binding) => Phase::Bound {
                        tool: binding,
                        status: OpenStatus::JustOpened,
                    },
                    None => {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "step executor ({current_function}): Open hop yielded no tool_name"
                        )));
                    }
                }
            }
            Phase::Bound { tool, .. } => match status {
                StepStatus::Finished => Phase::Terminal(TerminalReason::Finished),
                StepStatus::Aborted => Phase::Terminal(TerminalReason::Aborted),
                other => match OpenStatus::try_from_step(other) {
                    Some(os) => Phase::Bound { tool, status: os },
                    None => {
                        tracing::warn!(
                            status = other.as_str(),
                            "step_executor_loop: unexpected non-open status mapped to None"
                        );
                        Phase::Terminal(TerminalReason::MissingStatus)
                    }
                },
            },
            Phase::Terminal(_) => break,
        };

        if matches!(phase, Phase::Terminal(_)) {
            break;
        }
    }

    if matches!(phase, Phase::AwaitingOpen | Phase::Bound { .. }) && steps.len() >= max_steps {
        phase = Phase::Terminal(TerminalReason::MaxStepsExhausted);
    }

    let session_context = build_session_context(&phase);
    let selected_tool = phase.selected_tool().cloned();

    let step_exec_loop_elapsed = step_exec_loop_started_at.elapsed();
    metrics::record_step_executor_loop_duration(function_name, step_exec_loop_elapsed);
    metrics::record_step_executor_status(function_name, "done", step_exec_status_done_count);
    metrics::record_step_executor_status(
        function_name,
        "finished",
        step_exec_status_finished_count,
    );
    metrics::record_step_executor_status(function_name, "aborted", step_exec_status_aborted_count);

    tracing::info!(
        function = function_name,
        context_id = %scope.context_id().as_str(),
        message_id = %scope.message_id().as_str(),
        step_exec_loop_latency_ms_total = step_exec_loop_elapsed.as_millis() as u64,
        step_exec_llm_hop_count_total,
        step_exec_llm_hop_count_select,
        step_exec_llm_hop_count_act,
        step_exec_llm_hop_count_continue,
        step_exec_llm_hop_latency_ms_total,
        step_exec_llm_hop_latency_ms_select,
        step_exec_llm_hop_latency_ms_act,
        step_exec_llm_hop_latency_ms_continue,
        step_exec_status_done_count,
        step_exec_status_finished_count,
        step_exec_status_aborted_count,
        "step_executor_loop: summary"
    );

    Ok(StepExecutorResult {
        last,
        steps,
        session_context,
        selected_tool,
    })
}
