//! Planning domain types and resolver.
//!
//! **Wire vs resolved:** Shapes deserialized from `__execution_session_invoke` live in
//! [`crate::execution_session_types`] with a `*Wire` suffix (JSON DTOs). Types in *this* module are
//! what [`PlanningResolver`] validates before provenance effects—host bookkeeping (e.g. parsed
//! `PlanningSupersessionKind`, merged message ids) is applied before calling into the resolver.
//!
//! ## Citations vs opaque “evidence”
//!
//! Submissions carry **`citations: Vec<Citation>`** ([`baml_rt_core::Citation`]): validated ref-table
//! strings (`#N` session history, `@N` / `@N:L` archives). They ground intent and step transitions for
//! provenance and drift checks. **`derived_from_message_ids`** is **host bookkeeping** (execution-session
//! message UUID lineage): bound **only** from the Rust invocation scope — agent / QuickJS input is adversarial and
//! must not control sensitive ids (the wire omits this field; legacy JSON keys are ignored). Provenance
//! `IntentResolved` effects carry **citations** only (see `baml_rt_core::bus`).
//!
//! See **`docs/citable-history-and-checked-citations.md`**.

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Citation, Result,
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
    pub citations: Vec<Citation>,
    /// Host bookkeeping: execution-session message UUID lineage (host/shim wire field; merged from scope when omitted).
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
    pub citations: Vec<Citation>,
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
        if submission.citations.is_empty() {
            tracing::warn!(
                intent_id = %submission.intent_id.as_str(),
                "submitIntent: no citations provided — intent has no checked ref-table trail. \
                 Add a citations field to the BAML planning function return type."
            );
        }
        if submission.citations.is_empty() && submission.derived_from_message_ids.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "intent must include citations and/or host-bound derived_from_message_ids (message id from invocation scope)"
                    .to_string(),
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
        {
            return Err(BamlRtError::InvalidArgument(
                "plan step status change fields must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }
}
