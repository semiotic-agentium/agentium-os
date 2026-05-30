// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_core::Citation;

use super::{BamlRuntimeManager, manager_prelude::*, planning_emit};

impl BamlRuntimeManager {
    fn planning_emit_env(&self) -> planning_emit::PlanningEmitEnv<'_> {
        planning_emit::PlanningEmitEnv {
            planning_resolver: &self.state.planning_resolver,
            effect_emitter: &self.state.effect_emitter,
            tool_registry: &self.state.tool_registry,
            conversation_context_provider: &self.state.conversation_context_provider,
        }
    }

    pub async fn emit_planning_intent_resolved(
        &self,
        scope: &context::RuntimeScope,
        submission: IntentSubmission,
        epoch: Option<u64>,
    ) -> Result<()> {
        self.planning_emit_env()
            .emit_intent_resolved(scope, submission, epoch)
            .await
    }

    pub async fn emit_planning_plan_generated(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        plan_id: String,
        steps: Value,
        supersession: Option<PlanningSupersessionKind>,
        epoch: Option<u64>,
    ) -> Result<()> {
        self.planning_emit_env()
            .emit_plan_generated(scope, intent_id, plan_id, steps, supersession, epoch)
            .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "nine distinct planning fields with no natural grouping at this layer"
    )]
    pub async fn emit_planning_step_status_changed(
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
        self.planning_emit_env()
            .emit_step_status_changed(
                scope, intent_id, plan_id, step_id, old_status, new_status, citations, epoch,
            )
            .await
    }
}
