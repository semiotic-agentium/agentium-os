//! Planning → resolver → effect bus. Centralises intent / plan / step-status emission so tests
//! can drive [`PlanningEmitEnv`] with mock resolvers and emitters without constructing a full
//! [`super::BamlRuntimeManager`].

use std::sync::Arc;

use baml_rt_core::{
    BamlRtError, Citation, Outcome, Result,
    bus::{EffectEmitter, EffectEvent, PlanningSupersessionKind, ToolEffectMetadata},
    context,
    correlation::current_correlation_id,
    ids::{ExternalId, TaskId},
};
use baml_rt_tools::ToolRegistry as ConcreteToolRegistry;
use serde_json::Value;

use super::open_input;
use crate::{
    baml_execution::ConversationContextProvider,
    planning::{
        IntentSubmission, PlanStepStatusChange, PlanSubmission, PlanningDynamicContext,
        PlanningResolver,
    },
};

/// Injected dependencies for planning effect emission (resolver + bus + tool list + optional history).
pub(crate) struct PlanningEmitEnv<'a> {
    pub planning_resolver: &'a Arc<dyn PlanningResolver>,
    pub effect_emitter: &'a Option<Arc<dyn EffectEmitter>>,
    pub tool_registry: &'a Arc<ConcreteToolRegistry>,
    pub conversation_context_provider: &'a Option<Arc<dyn ConversationContextProvider>>,
}

impl PlanningEmitEnv<'_> {
    pub(crate) async fn build_dynamic_context(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<PlanningDynamicContext> {
        let mut available_tools = self
            .tool_registry
            .all_metadata()
            .iter()
            .map(|metadata| metadata.name.to_string())
            .collect::<Vec<_>>();
        available_tools.sort();
        available_tools.dedup();
        let conversation_history =
            if let Some(provider) = self.conversation_context_provider.as_ref() {
                provider.conversation_history_json(scope).await?
            } else {
                None
            };
        Ok(PlanningDynamicContext {
            scope: scope.clone(),
            available_tools,
            conversation_history,
        })
    }

    pub(crate) async fn emit_intent_resolved(
        &self,
        scope: &context::RuntimeScope,
        submission: IntentSubmission,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning intent requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_dynamic_context(scope).await?;
        let resolved = self
            .planning_resolver
            .resolve_intent(&dynamic_context, submission)
            .await?;
        let event = EffectEvent::IntentResolved {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: resolved.intent_id,
            description: resolved.description,
            citations: resolved.citations,
            supersession: resolved.supersession,
            epoch,
        };
        emitter.emit(event).await
    }

    pub(crate) async fn emit_plan_generated(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        plan_id: String,
        steps: Value,
        supersession: Option<PlanningSupersessionKind>,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning plan requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_dynamic_context(scope).await?;
        let resolved = self
            .planning_resolver
            .resolve_plan(
                &dynamic_context,
                PlanSubmission {
                    intent_id: intent_id.into(),
                    plan_id: plan_id.into(),
                    steps,
                    supersession,
                },
            )
            .await?;
        let event = EffectEvent::PlanGenerated {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: resolved.intent_id,
            plan_id: resolved.plan_id,
            steps: resolved.steps,
            supersession: resolved.supersession,
            epoch,
        };
        emitter.emit(event).await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn emit_step_status_changed(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        plan_id: String,
        step_id: String,
        old_status: Option<String>,
        new_status: String,
        citations: Vec<Citation>,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning step status requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_dynamic_context(scope).await?;
        let resolved = self
            .planning_resolver
            .resolve_step_status(
                &dynamic_context,
                PlanStepStatusChange {
                    intent_id: intent_id.into(),
                    plan_id: plan_id.into(),
                    step_id: step_id.into(),
                    old_status,
                    new_status,
                    citations: citations.clone(),
                },
            )
            .await?;
        if open_input::is_planning_step_terminal_completed_status(&resolved.new_status) {
            let context_id = scope.context_id().clone();
            let mut metadata_map = serde_json::Map::new();
            if let Some(correlation_id) = current_correlation_id() {
                metadata_map.insert(
                    "correlation_id".to_string(),
                    Value::String(correlation_id.to_string()),
                );
            }
            metadata_map.insert(
                "message_id".to_string(),
                Value::String(scope.message_id().as_str().to_owned()),
            );
            metadata_map.insert(
                "task_id".to_string(),
                Value::String(task_id.as_str().to_owned()),
            );
            metadata_map.insert(
                "agent_id".to_string(),
                Value::String(scope.agent_id().as_str().to_owned()),
            );
            metadata_map.insert(
                "plan_id".to_string(),
                Value::String(resolved.plan_id.as_str().to_string()),
            );
            metadata_map.insert(
                "step_id".to_string(),
                Value::String(resolved.step_id.as_str().to_string()),
            );
            metadata_map.insert(
                "phase".to_string(),
                Value::String("execution_session_complete".to_string()),
            );
            let tool_meta = ToolEffectMetadata {
                tool_name: "a2a/execution_session_step".to_string(),
                function_name: None,
                args: serde_json::json!({
                    "plan_id": resolved.plan_id.as_str(),
                    "step_id": resolved.step_id.as_str(),
                }),
                metadata: Value::Object(metadata_map),
                delegation_target: None,
                tool_backend: None,
                tool_digest: None,
            };
            let token = emitter.start_tool(context_id, tool_meta).await?;
            token
                .complete(
                    emitter.as_ref(),
                    0,
                    Outcome::Success,
                    Some(serde_json::json!({
                        "citations": resolved
                            .citations
                            .iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>(),
                    })),
                )
                .await?;
        }
        let event = EffectEvent::PlanStepStatusChanged {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: resolved.intent_id,
            plan_id: resolved.plan_id,
            step_id: resolved.step_id,
            old_status: resolved.old_status,
            new_status: resolved.new_status,
            citations: resolved.citations,
            epoch,
        };
        emitter.emit(event).await
    }
}
