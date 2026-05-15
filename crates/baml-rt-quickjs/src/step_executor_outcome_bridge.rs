//! Map step-executor loop results and errors into [`baml_rt_core::StepExecutorOutcome`].

use baml_rt_core::{BamlRtError, StepExecutorOutcome, StepPlanRecovery};

use crate::step_executor_loop::StepExecutorResult;

#[must_use]
pub(crate) fn step_executor_outcome_from_loop_result(
    result: Result<StepExecutorResult, BamlRtError>,
) -> StepExecutorOutcome {
    match result {
        Ok(r) => StepExecutorOutcome::Completed {
            last: r.last,
            steps: r.steps,
            session_context: r.session_context,
            selected_tool: r.selected_tool.map(|t| t.to_string()),
        },
        Err(e) => map_step_executor_error(e),
    }
}

fn map_step_executor_error(e: BamlRtError) -> StepExecutorOutcome {
    if let BamlRtError::StepPlanCorrectable(r) = e {
        return StepExecutorOutcome::AgentCorrectable { recovery: r };
    }
    if let Some(r) = legacy_step_plan_recovery(&e) {
        return StepExecutorOutcome::AgentCorrectable { recovery: r };
    }
    StepExecutorOutcome::Fatal {
        message: e.to_string(),
        code: Some("step_executor_fatal".to_string()),
    }
}

/// Deprecated string bridge for errors not yet emitted as [`BamlRtError::StepPlanCorrectable`].
fn legacy_step_plan_recovery(err: &BamlRtError) -> Option<StepPlanRecovery> {
    let BamlRtError::InvalidArgument(msg) = err else {
        return None;
    };
    const SR_MISSING_REF: &str =
        "SearchRead step: missing required archive_ref (expected e.g. \"@1\")";
    const PR_MISSING_REF: &str =
        "PageRead step: missing required archive_ref (expected e.g. \"@1\")";
    const SR_MISSING_GREP: &str =
        "SearchRead step: grep is required (non-empty line filter pattern)";
    const PR_GREP: &str =
        "PageRead step: grep must be omitted or empty; use SearchRead for line filtering";
    const LEGACY_READ: &str = "Legacy op \"Read\" was removed; emit \"SearchRead\" (requires grep) or \"PageRead\" (no grep) for archive access";
    const MISSING_OP: &str = "ToolSessionPlan step missing op";
    const SEND_NO_INPUT: &str = "Send step missing required input field";
    const SEND_NULL: &str = "Send step input must not be null — provide a non-empty object";
    const STEP_NOT_OBJ: &str = "ToolSessionPlan.step must be an object";

    if msg == SR_MISSING_REF {
        return Some(StepPlanRecovery::missing_archive_ref_search_read());
    }
    if msg == PR_MISSING_REF {
        return Some(StepPlanRecovery::missing_archive_ref_page_read());
    }
    if msg == SR_MISSING_GREP {
        return Some(StepPlanRecovery::missing_search_read_grep());
    }
    if msg == PR_GREP {
        return Some(StepPlanRecovery::page_read_grep_forbidden());
    }
    if msg == LEGACY_READ {
        return Some(StepPlanRecovery::legacy_read_op_removed());
    }
    if msg == MISSING_OP {
        return Some(StepPlanRecovery::tool_session_step_missing_op());
    }
    if msg == SEND_NO_INPUT {
        return Some(StepPlanRecovery::send_missing_input());
    }
    if msg == SEND_NULL {
        return Some(StepPlanRecovery::send_input_null());
    }
    if msg == STEP_NOT_OBJ {
        return Some(StepPlanRecovery::tool_session_plan_step_not_object());
    }
    const INVALID_GREP_PREFIX: &str = "SearchRead step: invalid grep pattern ";
    if let Some(rest) = msg.strip_prefix(INVALID_GREP_PREFIX) {
        return Some(StepPlanRecovery::invalid_search_read_grep(
            rest,
            "rejected by host",
        ));
    }
    let unknown_prefix = "Unknown tool session op ";
    if let Some(op) = msg.strip_prefix(unknown_prefix) {
        return Some(StepPlanRecovery::unknown_tool_session_op(op));
    }
    let plan_prefix = "Invalid plan input JSON: ";
    if let Some(detail) = msg.strip_prefix(plan_prefix) {
        return Some(StepPlanRecovery::invalid_plan_input_json(detail));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_invalid_argument_maps_to_agent_correctable() {
        let err = BamlRtError::InvalidArgument(
            "PageRead step: missing required archive_ref (expected e.g. \"@1\")".to_string(),
        );
        let out = step_executor_outcome_from_loop_result(Err(err));
        let StepExecutorOutcome::AgentCorrectable { recovery } = out else {
            panic!("expected AgentCorrectable, got {out:?}");
        };
        assert_eq!(recovery.code.as_str(), "missing_archive_ref_page_read");
    }

    #[test]
    fn fatal_survives_unknown_invalid_argument() {
        let err = BamlRtError::InvalidArgument("totally unknown".to_string());
        let out = step_executor_outcome_from_loop_result(Err(err));
        let StepExecutorOutcome::Fatal { message, .. } = out else {
            panic!("expected Fatal");
        };
        assert!(message.contains("totally unknown"));
    }
}
