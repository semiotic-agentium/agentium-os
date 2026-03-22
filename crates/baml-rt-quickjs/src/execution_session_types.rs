//! Typed execution-session command and response models for the JS↔Rust boundary.
//!
//! Replaces ad-hoc JSON parsing with serde-tagged enums for strict contract enforcement.
//!
//! **Wire vs planning:** `*Wire` types are JSON DTOs for `__execution_session_invoke`. After host
//! bookkeeping, map into [`crate::planning`] (`IntentSubmission`, `PlanSubmission`, …) for
//! [`crate::planning::PlanningResolver`].

use baml_rt_core::ids::{ExecutionSessionId, IntentId, PlanId, PlanStepId};
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
        evidence_text: String,
    },
    CompleteStep {
        session_id: ExecutionSessionId,
        step_id: PlanStepId,
        evidence_text: String,
    },
    Finish {
        session_id: ExecutionSessionId,
    },
    Abort {
        session_id: ExecutionSessionId,
        reason: String,
    },
}

/// Wire JSON body for `submit_intent` (`camelCase` keys). Map to [`crate::planning::IntentSubmission`]
/// after host lineage and supersession parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentSubmissionWire {
    pub intent_id: IntentId,
    pub description: String,
    /// Provenance lineage; when omitted (e.g. BAML omitted the field), the host fills from the
    /// active invocation scope's message id in `__execution_session_invoke` (`submit_intent`).
    #[serde(default)]
    pub derived_from_message_ids: Vec<String>,
    /// "replaced"|"replaced_by"|"replacedBy" -> ReplacedBy, "refined"|"refined_by"|"refinedBy" -> RefinedBy
    #[serde(default)]
    pub supersession: Option<String>,
}

/// Wire JSON body for `submit_plan`. Steps stay as typed rows here; the host builds a JSON array
/// for [`crate::planning::PlanSubmission::steps`] before calling the resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSubmissionWire {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub steps: Vec<PlanStepSubmission>,
    #[serde(default)]
    pub supersession: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStepSubmission {
    pub step_id: PlanStepId,
    pub description: String,
    pub order: u64,
    #[serde(default)]
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
                derived_from_message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
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
                derived_from_message_ids: vec!["msg-3".to_string()],
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
            evidence_text: "Beginning execution".to_string(),
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
            evidence_text: "Step completed successfully".to_string(),
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
                r#"{"action":"submit_intent","session_id":"s","intent":{"intentId":"i","description":"d","derivedFromMessageIds":[]}}"#,
            ),
            (
                "submit_intent_omits_derived_from_message_ids",
                r#"{"action":"submit_intent","session_id":"s","intent":{"intentId":"i","description":"d"}}"#,
            ),
            (
                "submit_plan",
                r#"{"action":"submit_plan","session_id":"s","plan":{"intentId":"i","planId":"p","steps":[]}}"#,
            ),
            (
                "start_step",
                r#"{"action":"start_step","session_id":"s","step_id":"x","evidence_text":"e"}"#,
            ),
            (
                "complete_step",
                r#"{"action":"complete_step","session_id":"s","step_id":"x","evidence_text":"e"}"#,
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
}
