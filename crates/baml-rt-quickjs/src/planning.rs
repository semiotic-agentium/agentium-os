//! Planning domain types and resolver.
//!
//! **Naming:** Shapes deserialized from `__execution_session_invoke` live in
//! [`crate::execution_session_types`] with a `*Wire` suffix (JSON DTOs). Types in *this* module
//! are what the [`PlanningResolver`] validates before provenance effects—same field semantics, but
//! host bookkeeping (e.g. parsed `PlanningSupersessionKind`, merged message ids) already applied
//! where applicable. There is no separate “canonical” family; contrast is **wire vs resolved**.

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

/// Resolved intent submission: passed to [`PlanningResolver`] and provenance after wire parse +
/// host lineage / supersession bookkeeping.
#[derive(Debug, Clone)]
pub struct IntentSubmission {
    pub intent_id: IntentId,
    pub description: String,
    pub derived_from_message_ids: Vec<String>,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct PlanSubmission {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub steps: Value,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct PlanStepStatusChange {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub step_id: PlanStepId,
    pub old_status: Option<String>,
    pub new_status: String,
    pub evidence_text: String,
}

#[async_trait]
pub trait PlanningResolver: Send + Sync {
    async fn resolve_intent(
        &self,
        context: &PlanningDynamicContext,
        submission: IntentSubmission,
    ) -> Result<IntentSubmission>;
    async fn resolve_plan(
        &self,
        context: &PlanningDynamicContext,
        submission: PlanSubmission,
    ) -> Result<PlanSubmission>;
    async fn resolve_step_status(
        &self,
        context: &PlanningDynamicContext,
        submission: PlanStepStatusChange,
    ) -> Result<PlanStepStatusChange>;
}

pub(crate) struct DefaultPlanningResolver;

#[async_trait]
impl PlanningResolver for DefaultPlanningResolver {
    async fn resolve_intent(
        &self,
        _context: &PlanningDynamicContext,
        submission: IntentSubmission,
    ) -> Result<IntentSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "intent_id must be non-empty".to_string(),
            ));
        }
        if submission.description.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "intent description must be non-empty".to_string(),
            ));
        }
        if submission.derived_from_message_ids.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "intent must derive from at least one message".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_plan(
        &self,
        _context: &PlanningDynamicContext,
        submission: PlanSubmission,
    ) -> Result<PlanSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "plan intent_id must be non-empty".to_string(),
            ));
        }
        if submission.plan_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "plan_id must be non-empty".to_string(),
            ));
        }
        let Some(steps) = submission.steps.as_array() else {
            return Err(BamlRtError::InvalidArgument(
                "plan steps must be a JSON array".to_string(),
            ));
        };
        if steps.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "plan steps must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_step_status(
        &self,
        _context: &PlanningDynamicContext,
        submission: PlanStepStatusChange,
    ) -> Result<PlanStepStatusChange> {
        if submission.intent_id.as_str().trim().is_empty()
            || submission.plan_id.as_str().trim().is_empty()
            || submission.step_id.as_str().trim().is_empty()
            || submission.new_status.trim().is_empty()
            || submission.evidence_text.trim().is_empty()
        {
            return Err(BamlRtError::InvalidArgument(
                "plan step status change fields must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }
}
