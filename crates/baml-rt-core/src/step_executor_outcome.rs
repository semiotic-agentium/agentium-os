//! Discriminated wire outcome for [`crate::error::BamlRtError::StepPlanCorrectable`] and
//! `__run_step_executor` / `runGeneratedStepExecutor`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::semantics::ErrorDisposition;

/// Maximum structured correction steps attached to a recovery payload.
pub const MAX_STEP_PLAN_FIX_STEPS: usize = 4;

/// Stable machine key for plan / archive-read mistakes the LLM can correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPlanViolationCode {
    MissingArchiveRefPageRead,
    MissingArchiveRefSearchRead,
    MissingSearchReadGrep,
    InvalidSearchReadGrep,
    PageReadGrepForbidden,
    LegacyReadOpRemoved,
    ToolSessionStepMissingOp,
    ToolSessionPlanStepNotObject,
    SendMissingInput,
    SendInputNull,
    UnknownToolSessionOp,
    InvalidPlanInputJson,
}

impl StepPlanViolationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingArchiveRefPageRead => "missing_archive_ref_page_read",
            Self::MissingArchiveRefSearchRead => "missing_archive_ref_search_read",
            Self::MissingSearchReadGrep => "missing_search_read_grep",
            Self::InvalidSearchReadGrep => "invalid_search_read_grep",
            Self::PageReadGrepForbidden => "page_read_grep_forbidden",
            Self::LegacyReadOpRemoved => "legacy_read_op_removed",
            Self::ToolSessionStepMissingOp => "tool_session_step_missing_op",
            Self::ToolSessionPlanStepNotObject => "tool_session_plan_step_not_object",
            Self::SendMissingInput => "send_missing_input",
            Self::SendInputNull => "send_input_null",
            Self::UnknownToolSessionOp => "unknown_tool_session_op",
            Self::InvalidPlanInputJson => "invalid_plan_input_json",
        }
    }
}

/// Structured host guidance for a malformed session-plan step (LLM-visible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{mistake}")]
pub struct StepPlanRecovery {
    pub code: StepPlanViolationCode,
    pub disposition: ErrorDisposition,
    pub mistake: String,
    pub invariant: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fix_steps: Vec<String>,
}

impl StepPlanRecovery {
    /// Builds recovery with at most [`MAX_STEP_PLAN_FIX_STEPS`] steps (extra entries dropped).
    #[must_use]
    pub fn new(
        code: StepPlanViolationCode,
        mistake: impl Into<String>,
        invariant: impl Into<String>,
        fix_steps: Vec<String>,
    ) -> Self {
        let mut fix_steps = fix_steps;
        if fix_steps.len() > MAX_STEP_PLAN_FIX_STEPS {
            fix_steps.truncate(MAX_STEP_PLAN_FIX_STEPS);
        }
        Self {
            code,
            disposition: ErrorDisposition::LlmCorrectable,
            mistake: mistake.into(),
            invariant: invariant.into(),
            fix_steps,
        }
    }

    #[must_use]
    pub fn missing_archive_ref_page_read() -> Self {
        Self::new(
            StepPlanViolationCode::MissingArchiveRefPageRead,
            "PageRead was emitted without a visible archive reference (@N).",
            "PageRead.input.archive_ref is required and must be a short ref like \"@1\" from a prior Send in this turn.",
            vec![
                "Locate the @N header from the last successful Send in conversation history."
                    .to_string(),
                "Re-emit PageRead with input.archive_ref set to that @N.".to_string(),
            ],
        )
    }

    #[must_use]
    pub fn missing_archive_ref_search_read() -> Self {
        Self::new(
            StepPlanViolationCode::MissingArchiveRefSearchRead,
            "SearchRead was emitted without a visible archive reference (@N).",
            "SearchRead.input.archive_ref is required and must be a short ref like \"@1\" from a prior Send in this turn.",
            vec![
                "Locate the @N header from the last successful Send in conversation history.".to_string(),
                "Re-emit SearchRead with input.archive_ref set to that @N and a non-empty grep pattern.".to_string(),
            ],
        )
    }

    #[must_use]
    pub fn missing_search_read_grep() -> Self {
        Self::new(
            StepPlanViolationCode::MissingSearchReadGrep,
            "SearchRead was emitted without a line-filter pattern.",
            "SearchRead.input.grep is required (non-empty) — use PageRead when you need full page text without filtering.",
            vec!["Provide a non-empty grep string matching lines to return.".to_string()],
        )
    }

    #[must_use]
    pub fn invalid_search_read_grep(pattern: &str, detail: &str) -> Self {
        Self::new(
            StepPlanViolationCode::InvalidSearchReadGrep,
            format!("SearchRead grep pattern is invalid ({pattern:?}): {detail}"),
            "SearchRead.input.grep must be a valid line filter accepted by the host.",
            vec!["Fix the grep pattern syntax and retry.".to_string()],
        )
    }

    #[must_use]
    pub fn page_read_grep_forbidden() -> Self {
        Self::new(
            StepPlanViolationCode::PageReadGrepForbidden,
            "PageRead included a grep field; PageRead is for full slices only.",
            "PageRead must not set input.grep — use SearchRead for line filtering.",
            vec![
                "Remove grep from PageRead, or switch the op to SearchRead with grep + archive_ref."
                    .to_string(),
            ],
        )
    }

    #[must_use]
    pub fn legacy_read_op_removed() -> Self {
        Self::new(
            StepPlanViolationCode::LegacyReadOpRemoved,
            "Legacy op \"Read\" is not supported.",
            "Emit SearchRead (requires grep) or PageRead (no grep) for archive access.",
            vec![
                "Replace Read with SearchRead or PageRead following the archive-read contract."
                    .to_string(),
            ],
        )
    }

    #[must_use]
    pub fn tool_session_step_missing_op() -> Self {
        Self::new(
            StepPlanViolationCode::ToolSessionStepMissingOp,
            "A tool session plan step was emitted without an `op` field.",
            "Every session-plan step must include op: Open | Send | SearchRead | PageRead | Finish | Abort.",
            vec!["Emit a valid op string for this hop.".to_string()],
        )
    }

    #[must_use]
    pub fn tool_session_plan_step_not_object() -> Self {
        Self::new(
            StepPlanViolationCode::ToolSessionPlanStepNotObject,
            "ToolSessionPlan.step was not a JSON object.",
            "ToolSessionPlan.step must be an object describing one session-plan hop.",
            vec!["Emit step as an object with op and required fields.".to_string()],
        )
    }

    #[must_use]
    pub fn send_missing_input() -> Self {
        Self::new(
            StepPlanViolationCode::SendMissingInput,
            "Send step was emitted without the required input object.",
            "Send requires a non-null `input` object for the host tool session.",
            vec!["Provide Send.input as a non-empty object.".to_string()],
        )
    }

    #[must_use]
    pub fn send_input_null() -> Self {
        Self::new(
            StepPlanViolationCode::SendInputNull,
            "Send step input was null.",
            "Send.input must not be null — provide a non-empty object.",
            vec!["Populate Send.input before emitting Send.".to_string()],
        )
    }

    #[must_use]
    pub fn unknown_tool_session_op(op: &str) -> Self {
        Self::new(
            StepPlanViolationCode::UnknownToolSessionOp,
            format!("Unknown tool session op {op:?}."),
            "op must be one of the supported session-plan operations for this host.",
            vec!["Emit a supported op for the current FSM phase.".to_string()],
        )
    }

    #[must_use]
    pub fn invalid_plan_input_json(detail: &str) -> Self {
        Self::new(
            StepPlanViolationCode::InvalidPlanInputJson,
            format!("Plan input JSON could not be parsed: {detail}"),
            "Plan inputs that are strings must contain valid JSON when parsed.",
            vec!["Fix the JSON syntax in the plan input.".to_string()],
        )
    }
}

/// Witness returned from `__run_step_executor` — always discriminated; never ambiguous flat JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StepExecutorOutcome {
    Completed {
        last: Value,
        steps: Vec<Value>,
        session_context: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        selected_tool: Option<String>,
    },
    AgentCorrectable {
        recovery: StepPlanRecovery,
    },
    Fatal {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_executor_outcome_completed_json_stable() {
        let v = StepExecutorOutcome::Completed {
            last: serde_json::json!({"status": "done"}),
            steps: vec![serde_json::json!({"op": "open"})],
            session_context: serde_json::json!({"contract_version": "session_context_v2"}),
            selected_tool: Some("support/notion".to_string()),
        };
        let json = serde_json::to_value(&v).expect("serialize");
        insta::assert_json_snapshot!(json);
    }

    #[test]
    fn step_executor_outcome_agent_correctable_json_stable() {
        let v = StepExecutorOutcome::AgentCorrectable {
            recovery: StepPlanRecovery::missing_archive_ref_page_read(),
        };
        let json = serde_json::to_value(&v).expect("serialize");
        insta::assert_json_snapshot!(json);
    }

    #[test]
    fn step_executor_outcome_fatal_json_stable() {
        let v = StepExecutorOutcome::Fatal {
            message: "phase function missing".to_string(),
            code: Some("step_executor_fatal".to_string()),
        };
        let json = serde_json::to_value(&v).expect("serialize");
        insta::assert_json_snapshot!(json);
    }
}
