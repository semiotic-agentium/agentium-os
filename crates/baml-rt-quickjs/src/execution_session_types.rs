//! Typed execution-session command and response models for the JS↔Rust boundary.
//!
//! Replaces ad-hoc JSON parsing with serde-tagged enums for strict contract enforcement.
//!
//! **`IntentSubmissionWire` / `PlanSubmissionWire`:** JSON DTOs from `__execution_session_invoke`.
//! The host maps wire → [`crate::planning::IntentSubmission`] / [`crate::planning::PlanSubmission`]
//! (parsed supersession, etc.) before resolver and effects.
//!
//! **Trust boundary:** agent / QuickJS code is treated as **adversarial**. Sensitive identifiers such as
//! execution-session **message UUID lineage** are **not** accepted from the wire; the host binds
//! [`crate::planning::IntentSubmission`]'s `derived_from_message_ids` solely from the active Rust
//! invocation scope (see `baml_registration`). A legacy `derivedFromMessageIds`
//! JSON key, if present, is **ignored** by serde.
//!
//! **BAML interop:** `PlanSubmissionWire` / `PlanStepSubmission` also accept **snake_case** keys
//! (`intent_id`, `plan_id`, `step_id`, `depends_on`) via serde `alias`, so nested `plan` objects can
//! be built from BAML-shaped step structs without renaming fields in TypeScript.
//!
//! **Planning identifiers are not global provenance keys:** strings on this wire (`intentId`,
//! `planId`, `stepId`) are **task-scoped planning aliases** (often agent-chosen or LLM-authored
//! slugs). The host derives canonical graph entity ids by compounding them with `task_id` (and
//! related scope); agents must never treat these strings as durable global identifiers.

use baml_rt_core::{
    Citation,
    ids::{ExecutionSessionId, IntentId, PlanId, PlanStepId},
};
use serde::{Deserialize, Serialize};

/// Serde-tagged command envelope for `__execution_session_invoke` payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExecutionSessionCommand {
    Open,
    SubmitIntent {
        session_id: ExecutionSessionId,
        intent: IntentSubmissionWire,
    },
    SubmitPlan {
        session_id: ExecutionSessionId,
        plan: PlanSubmissionWire,
    },
    StartStep {
        session_id: ExecutionSessionId,
        step_id: PlanStepId,
        #[serde(default)]
        citations: Vec<Citation>,
    },
    CompleteStep {
        session_id: ExecutionSessionId,
        step_id: PlanStepId,
        #[serde(default)]
        citations: Vec<Citation>,
    },
    Finish {
        session_id: ExecutionSessionId,
    },
    Abort {
        session_id: ExecutionSessionId,
        reason: String,
    },
}

/// Wire DTO: `intent_id` is a **planning alias** for this task’s execution session, not a global id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentSubmissionWire {
    #[serde(alias = "intent_id")]
    pub intent_id: IntentId,
    pub description: String,
    /// Citation refs (`#N` / `@N`) for the history entries this intent was derived from.
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// "replaced"|"replaced_by"|"replacedBy" -> ReplacedBy, "refined"|"refined_by"|"refinedBy" -> RefinedBy
    #[serde(default)]
    pub supersession: Option<String>,
}

/// Wire DTO: `intent_id` / `plan_id` are **task-scoped planning aliases**, not global provenance ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSubmissionWire {
    #[serde(alias = "intent_id")]
    pub intent_id: IntentId,
    #[serde(alias = "plan_id")]
    pub plan_id: PlanId,
    pub steps: Vec<PlanStepSubmission>,
    #[serde(default)]
    pub supersession: Option<String>,
}

/// Wire DTO: `step_id` is a **plan-local alias** (may be LLM-authored); canonical step entities
/// compound `task_id` + `plan_id` + this string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStepSubmission {
    #[serde(alias = "step_id")]
    pub step_id: PlanStepId,
    pub description: String,
    pub order: u64,
    #[serde(default, alias = "depends_on")]
    pub depends_on: Vec<String>,
}

/// Response envelope for execution-session actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSessionResponse {
    pub session_id: ExecutionSessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

#[cfg(test)]
mod tests {
    //! Serde round-trip snapshots for `ExecutionSessionCommand`.
    //!
    //! These snapshots document the wire format emitted by the JS shim and consumed by
    //! the Rust host. Any rename of a field or variant causes a snapshot diff, making
    //! accidental protocol breaks visible immediately.

    use std::str::FromStr;

    use super::*;

    fn session_id() -> ExecutionSessionId {
        ExecutionSessionId::new("session-test-123".to_string())
    }

    fn intent_id() -> IntentId {
        IntentId::from("intent-test-1".to_string())
    }

    fn plan_id() -> PlanId {
        PlanId::from("plan-test-1".to_string())
    }

    fn step_id() -> PlanStepId {
        PlanStepId::from("step-test-1".to_string())
    }

    #[test]
    fn snapshot_open() {
        let cmd = ExecutionSessionCommand::Open;
        insta::assert_json_snapshot!(
            "execution_session_command_open",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_submit_intent() {
        let cmd = ExecutionSessionCommand::SubmitIntent {
            session_id: session_id(),
            intent: IntentSubmissionWire {
                intent_id: intent_id(),
                description: "Investigate the anomaly".to_string(),
                citations: vec![
                    Citation::from_str("#1").unwrap(),
                    Citation::from_str("#2").unwrap(),
                ],
                supersession: None,
            },
        };
        insta::assert_json_snapshot!(
            "execution_session_command_submit_intent",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_submit_intent_with_supersession() {
        let cmd = ExecutionSessionCommand::SubmitIntent {
            session_id: session_id(),
            intent: IntentSubmissionWire {
                intent_id: intent_id(),
                description: "Refined intent".to_string(),
                citations: vec![Citation::from_str("#3").unwrap()],
                supersession: Some("replaced_by".to_string()),
            },
        };
        insta::assert_json_snapshot!(
            "execution_session_command_submit_intent_supersession",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_submit_plan() {
        let cmd = ExecutionSessionCommand::SubmitPlan {
            session_id: session_id(),
            plan: PlanSubmissionWire {
                intent_id: intent_id(),
                plan_id: plan_id(),
                steps: vec![
                    PlanStepSubmission {
                        step_id: step_id(),
                        description: "Run diagnostics".to_string(),
                        order: 0,
                        depends_on: vec![],
                    },
                    PlanStepSubmission {
                        step_id: PlanStepId::from("step-test-2".to_string()),
                        description: "Apply fix".to_string(),
                        order: 1,
                        depends_on: vec!["step-test-1".to_string()],
                    },
                ],
                supersession: None,
            },
        };
        insta::assert_json_snapshot!(
            "execution_session_command_submit_plan",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_start_step() {
        let cmd = ExecutionSessionCommand::StartStep {
            session_id: session_id(),
            step_id: step_id(),
            citations: vec![Citation::from_str("#1").unwrap()],
        };
        insta::assert_json_snapshot!(
            "execution_session_command_start_step",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_complete_step() {
        let cmd = ExecutionSessionCommand::CompleteStep {
            session_id: session_id(),
            step_id: step_id(),
            citations: vec![
                Citation::from_str("#1").unwrap(),
                Citation::from_str("@4:L2").unwrap(),
            ],
        };
        insta::assert_json_snapshot!(
            "execution_session_command_complete_step",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_finish() {
        let cmd = ExecutionSessionCommand::Finish {
            session_id: session_id(),
        };
        insta::assert_json_snapshot!(
            "execution_session_command_finish",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    #[test]
    fn snapshot_abort() {
        let cmd = ExecutionSessionCommand::Abort {
            session_id: session_id(),
            reason: "Run function threw an error".to_string(),
        };
        insta::assert_json_snapshot!(
            "execution_session_command_abort",
            serde_json::to_value(&cmd).unwrap()
        );
    }

    /// Deserialise each snapshot back and assert the action tag round-trips correctly.
    #[test]
    fn round_trip_all_variants() {
        let cases: &[(&str, &str)] = &[
            ("open", r#"{"action":"open"}"#),
            (
                "submit_intent",
                "{\"action\":\"submit_intent\",\"session_id\":\"s\",\"intent\":{\"intentId\":\"i\",\"description\":\"d\",\"citations\":[\"#1\"]}}",
            ),
            (
                "submit_plan",
                r#"{"action":"submit_plan","session_id":"s","plan":{"intentId":"i","planId":"p","steps":[]}}"#,
            ),
            (
                "start_step",
                "{\"action\":\"start_step\",\"session_id\":\"s\",\"step_id\":\"x\",\"citations\":[\"#1\"]}",
            ),
            (
                "complete_step",
                "{\"action\":\"complete_step\",\"session_id\":\"s\",\"step_id\":\"x\",\"citations\":[\"#1\"]}",
            ),
            ("finish", r#"{"action":"finish","session_id":"s"}"#),
            (
                "abort",
                r#"{"action":"abort","session_id":"s","reason":"r"}"#,
            ),
        ];
        for (name, json_str) in cases {
            let cmd: ExecutionSessionCommand = serde_json::from_str(json_str)
                .unwrap_or_else(|e| panic!("round-trip failed for {name}: {e}\ninput: {json_str}"));
            let re_serialised = serde_json::to_string(&cmd)
                .unwrap_or_else(|e| panic!("re-serialise failed for {name}: {e}"));
            let reparsed: ExecutionSessionCommand = serde_json::from_str(&re_serialised)
                .unwrap_or_else(|e| panic!("second parse failed for {name}: {e}"));
            // Verify the action tag is preserved (compare serialised forms)
            assert_eq!(
                serde_json::to_value(&cmd).unwrap().get("action"),
                serde_json::to_value(&reparsed).unwrap().get("action"),
                "action tag mismatch for {name}"
            );
        }
    }

    #[test]
    fn deserialize_rejects_non_ref_citation() {
        let json =
            r#"{"action":"start_step","session_id":"s","step_id":"x","citations":["not-a-ref"]}"#;
        assert!(
            serde_json::from_str::<ExecutionSessionCommand>(json).is_err(),
            "invalid citation must fail serde deserialize"
        );
    }

    /// Adversarial agent JSON may include `derivedFromMessageIds`; it must not deserialize into wire state.
    #[test]
    fn deserialize_submit_intent_ignores_derived_from_message_ids() {
        let json = r##"{"action":"submit_intent","session_id":"s","intent":{"intentId":"i","description":"d","derivedFromMessageIds":["fake-uuid"],"citations":["#1"]}}"##;
        let cmd: ExecutionSessionCommand = serde_json::from_str(json).expect("must parse");
        let ExecutionSessionCommand::SubmitIntent { intent, .. } = cmd else {
            panic!("expected SubmitIntent");
        };
        assert_eq!(intent.intent_id.as_str(), "i");
        assert_eq!(intent.description, "d");
        assert_eq!(intent.citations.len(), 1);
    }

    /// BAML-shaped `plan` (snake_case keys) round-trips for submit_plan — no TS rename layer required.
    #[test]
    fn deserialize_submit_plan_accepts_snake_case_nested_keys() {
        let json = r##"{
            "action":"submit_plan",
            "session_id":"session-test-123",
            "plan":{
                "intent_id":"intent-i",
                "plan_id":"plan-p",
                "steps":[
                    {
                        "step_id":"a",
                        "description":"First",
                        "order":0,
                        "depends_on":[]
                    },
                    {
                        "step_id":"b",
                        "description":"Second",
                        "order":1,
                        "depends_on":["a"]
                    }
                ]
            }
        }"##;
        let cmd: ExecutionSessionCommand =
            serde_json::from_str(json).expect("parse snake_case plan");
        let ExecutionSessionCommand::SubmitPlan { plan, .. } = cmd else {
            panic!("expected SubmitPlan");
        };
        assert_eq!(plan.intent_id.as_str(), "intent-i");
        assert_eq!(plan.plan_id.as_str(), "plan-p");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].step_id.as_str(), "a");
        assert_eq!(plan.steps[0].depends_on.len(), 0);
        assert_eq!(plan.steps[1].step_id.as_str(), "b");
        assert_eq!(plan.steps[1].depends_on, vec!["a".to_string()]);
    }
}
