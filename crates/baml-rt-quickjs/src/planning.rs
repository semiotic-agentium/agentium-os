//! Planning domain types and canonical resolver.
//!
//! Defines the intent/plan/step lifecycle protocol used by the execution session FSM.
//! The `PlanningCanonicalResolver` trait validates and normalises submissions
//! before they are emitted as provenance events.

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    bus::PlanningSupersessionKind,
    context,
    ids::{IntentId, PlanId, PlanStepId},
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct PlanningDynamicContext {
    pub scope: context::RuntimeScope,
    pub available_tools: Vec<String>,
    pub conversation_history: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct CanonicalIntentSubmission {
    pub intent_id: IntentId,
    pub description: String,
    pub derived_from_message_ids: Vec<String>,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct CanonicalPlanSubmission {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub steps: Value,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct CanonicalPlanStepStatusChange {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub step_id: PlanStepId,
    pub old_status: Option<String>,
    pub new_status: String,
    pub evidence_text: String,
}

#[async_trait]
pub trait PlanningCanonicalResolver: Send + Sync {
    async fn resolve_intent(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalIntentSubmission,
    ) -> Result<CanonicalIntentSubmission>;
    async fn resolve_plan(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalPlanSubmission,
    ) -> Result<CanonicalPlanSubmission>;
    async fn resolve_step_status(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalPlanStepStatusChange,
    ) -> Result<CanonicalPlanStepStatusChange>;
}

pub(crate) struct DefaultPlanningCanonicalResolver;

#[async_trait]
impl PlanningCanonicalResolver for DefaultPlanningCanonicalResolver {
    async fn resolve_intent(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalIntentSubmission,
    ) -> Result<CanonicalIntentSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent_id must be non-empty".to_string(),
            ));
        }
        if submission.description.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent description must be non-empty".to_string(),
            ));
        }
        if submission.derived_from_message_ids.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent must derive from at least one message".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_plan(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalPlanSubmission,
    ) -> Result<CanonicalPlanSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan intent_id must be non-empty".to_string(),
            ));
        }
        if submission.plan_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan_id must be non-empty".to_string(),
            ));
        }
        let Some(steps) = submission.steps.as_array() else {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan steps must be an array".to_string(),
            ));
        };
        if steps.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan steps must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_step_status(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalPlanStepStatusChange,
    ) -> Result<CanonicalPlanStepStatusChange> {
        if submission.intent_id.as_str().trim().is_empty()
            || submission.plan_id.as_str().trim().is_empty()
            || submission.step_id.as_str().trim().is_empty()
            || submission.new_status.trim().is_empty()
            || submission.evidence_text.trim().is_empty()
        {
            return Err(BamlRtError::InvalidArgument(
                "canonical step status change fields must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }
}
