// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_interceptor::{
    InterceptorDecision, LLMCallContext, LLMInterceptor, ToolCallContext, ToolInterceptor,
};
use serde_json::Value;
use tracing::warn;

use crate::{
    config::SemioticPolicy,
    covers::covers_match,
    gate::{AmbiguityAwareGate, GateAction, GatePolicy},
    gate_outcome::GateOutcome,
    global::{
        global_denied_recent_store, global_gate_outcome_store, global_grounding_store,
        global_pending_gate_auth_store, resolve_semiotic_policy,
    },
    lint::lint,
    postcondition::run_postconditions,
    store::GroundingStore,
    telemetry::GateReasonCode,
    tier::{Tier, ToolTierMeta, classify_tier},
    trojan,
};

const ARTIFACT_TTL: Duration = Duration::from_secs(30 * 60);
const UNKNOWN_AGENT_PACKAGE: &str = "unknown";

/// Pre-tool semiotic gate interceptor.
pub struct SemioticToolInterceptor {
    pub store: Arc<GroundingStore>,
    gate: AmbiguityAwareGate,
}

struct GateVerdict {
    tier: Tier,
    reason: GateReasonCode,
    block_msg: Option<String>,
}

impl SemioticToolInterceptor {
    pub fn new(store: Arc<GroundingStore>) -> Self {
        Self {
            store,
            gate: AmbiguityAwareGate::default(),
        }
    }

    fn config_for(&self, context: &ToolCallContext) -> SemioticPolicy {
        let pkg = context.agent_package.as_deref();
        if pkg == Some(UNKNOWN_AGENT_PACKAGE) {
            tracing::debug!("semiotic policy: agent_package is placeholder 'unknown'");
        }
        resolve_semiotic_policy(pkg)
    }

    fn evaluate(
        &self,
        ctx: &ToolCallContext,
        meta: &ToolTierMeta,
        policy: &SemioticPolicy,
    ) -> GateVerdict {
        let tier = classify_tier(meta);
        if tier.as_u8() <= 1 {
            return GateVerdict {
                tier,
                reason: GateReasonCode::TierBelowGate,
                block_msg: None,
            };
        }

        let Some(artifact_raw) = self.store.get_live(&ctx.runtime_scope, ARTIFACT_TTL) else {
            return GateVerdict {
                tier,
                reason: GateReasonCode::NoArtifact,
                block_msg: Some(format!(
                    "Gate holds (tier {}: {}). Submit grounding via submitGrounding. Deficient: {}",
                    tier.as_u8(),
                    "no artifact",
                    "all critical/ambiguous nodes"
                )),
            };
        };

        let art = lint(artifact_raw);
        if !covers_match(&art.covers, &ctx.tool_name, &ctx.args) {
            return GateVerdict {
                tier,
                reason: GateReasonCode::CoversMismatch,
                block_msg: Some(format!(
                    "Gate holds (tier {}: {}). Fix covers patterns.",
                    tier.as_u8(),
                    "covers mismatch"
                )),
            };
        }

        let needs_postconditions =
            tier.as_u8() >= 2 && (tier != Tier::Irreversible || policy.require_postconditions_t3);
        if needs_postconditions && art.postconditions.is_empty() {
            return GateVerdict {
                tier,
                reason: GateReasonCode::NoPostconditions,
                block_msg: Some(format!(
                    "Gate holds (tier {}: {}). Add postconditions.",
                    tier.as_u8(),
                    "missing postconditions"
                )),
            };
        }

        let decision = self.gate.decide(&art, tier);
        if !decision.requests.is_empty() {
            let deficits = decision.requests.join(", ");
            return GateVerdict {
                tier,
                reason: GateReasonCode::DeficientNodes,
                block_msg: Some(format!(
                    "Gate holds (tier {}: deficient nodes). Ground: {}",
                    tier.as_u8(),
                    deficits
                )),
            };
        }

        match decision.action {
            GateAction::QueueForHuman => GateVerdict {
                tier,
                reason: GateReasonCode::Tier3Authorization,
                block_msg: Some("Tier-3 grounded: human authorization required.".into()),
            },
            _ => GateVerdict {
                tier,
                reason: GateReasonCode::RequirementsMet,
                block_msg: None,
            },
        }
    }

    fn decision_label(verdict: &GateVerdict, artifact_has_postconditions: bool) -> &'static str {
        if verdict.reason != GateReasonCode::RequirementsMet {
            return "deny";
        }
        if verdict.tier == Tier::Irreversible {
            return "ask";
        }
        if artifact_has_postconditions {
            "pass_gated"
        } else {
            "pass"
        }
    }

    fn apply_policy(
        policy: &SemioticPolicy,
        verdict: &GateVerdict,
        ctx: &ToolCallContext,
    ) -> InterceptorDecision {
        if verdict.reason == GateReasonCode::RequirementsMet {
            if verdict.tier == Tier::Irreversible && policy.should_enforce(verdict.tier.as_u8()) {
                let prompt = Self::authorization_prompt(ctx, verdict.tier.as_u8());
                global_pending_gate_auth_store().set_pending(
                    &ctx.runtime_scope,
                    &ctx.tool_name,
                    &ctx.args,
                );
                return InterceptorDecision::RequireAuthorization(prompt);
            }
            if !policy.should_enforce(verdict.tier.as_u8()) {
                tracing::debug!(tier = verdict.tier.as_u8(), "semiotic gate dry-run pass");
            }
            return InterceptorDecision::Allow;
        }

        if policy.should_enforce(verdict.tier.as_u8()) {
            global_denied_recent_store().record_denied(
                &ctx.runtime_scope,
                &ctx.tool_name,
                &ctx.args,
                verdict.tier.as_u8(),
            );
            return InterceptorDecision::Block(
                verdict
                    .block_msg
                    .clone()
                    .unwrap_or_else(|| verdict.reason.as_str().into()),
            );
        }

        tracing::debug!(
            tier = verdict.tier.as_u8(),
            reason = verdict.reason.as_str(),
            "semiotic gate dry-run would deny"
        );
        InterceptorDecision::Allow
    }

    fn authorization_prompt(ctx: &ToolCallContext, tier: u8) -> String {
        let store = global_grounding_store();
        let summary = store
            .get_live(&ctx.runtime_scope, ARTIFACT_TTL)
            .map(|a| a.instruction.clone())
            .unwrap_or_else(|| "Grounded tier-3 action".into());
        let post_count = store
            .get_live(&ctx.runtime_scope, ARTIFACT_TTL)
            .map(|a| a.postconditions.len())
            .unwrap_or(0);
        format!(
            "Tier-{tier} authorization required.\n\nGrounded intent: {summary}\nPostconditions declared: {post_count}\n\nReply to approve and continue execution."
        )
    }

    fn record_outcome(&self, ctx: &ToolCallContext, verdict: &GateVerdict, decision_label: &str) {
        global_gate_outcome_store().record(
            &ctx.runtime_scope,
            GateOutcome::new(
                &ctx.tool_name,
                verdict.tier.as_u8(),
                decision_label,
                verdict.reason,
                vec![],
            ),
        );
    }

    fn register_postconditions_if_pass(&self, ctx: &ToolCallContext, verdict: &GateVerdict) {
        if verdict.reason != GateReasonCode::RequirementsMet {
            return;
        }
        let Some(artifact) = self.store.get_live(&ctx.runtime_scope, ARTIFACT_TTL) else {
            return;
        };
        if artifact.postconditions.is_empty() {
            return;
        }
        let cwd = ctx
            .metadata
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        self.store.register_pending_postconditions(
            &ctx.runtime_scope,
            artifact.postconditions.clone(),
            verdict.tier.as_u8(),
            cwd,
        );
    }
}

#[async_trait]
impl ToolInterceptor for SemioticToolInterceptor {
    async fn intercept_tool_call(&self, context: &ToolCallContext) -> Result<InterceptorDecision> {
        let config = self.config_for(context);
        if !config.enabled {
            return Ok(InterceptorDecision::Allow);
        }

        if global_pending_gate_auth_store().is_granted(
            &context.runtime_scope,
            &context.tool_name,
            &context.args,
        ) {
            let verdict = GateVerdict {
                tier: Tier::Irreversible,
                reason: GateReasonCode::RequirementsMet,
                block_msg: None,
            };
            self.record_outcome(context, &verdict, "pass_gated");
            self.register_postconditions_if_pass(context, &verdict);
            return Ok(InterceptorDecision::Allow);
        }

        let meta = tool_meta_from_context(context);
        let verdict = self.evaluate(context, &meta, &config);

        if verdict.tier.as_u8() <= 1 {
            return Ok(InterceptorDecision::Allow);
        }

        let artifact_has_postconditions = self
            .store
            .get_live(&context.runtime_scope, ARTIFACT_TTL)
            .is_some_and(|a| !a.postconditions.is_empty());
        let decision_label = Self::decision_label(&verdict, artifact_has_postconditions);

        self.record_outcome(context, &verdict, decision_label);

        if verdict.reason == GateReasonCode::RequirementsMet {
            self.register_postconditions_if_pass(context, &verdict);
        }

        Ok(Self::apply_policy(&config, &verdict, context))
    }

    async fn stamp_tool_metadata(&self, context: &ToolCallContext, metadata: &mut Value) {
        let Some(mut gate) = global_gate_outcome_store().take(&context.runtime_scope) else {
            return;
        };
        let Value::Object(obj) = metadata else {
            return;
        };
        if gate.decision == "ask" {
            gate.gate_authorization = Some(true);
        }
        if let Ok(v) = serde_json::to_value(gate) {
            obj.insert("semiotic_gate".to_string(), v);
        }
    }

    async fn on_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<serde_json::Value>,
        _duration_ms: u64,
    ) {
        let config = self.config_for(context);
        if !config.enabled {
            return;
        }
        if result.is_err() {
            return;
        }
        let Some(pending) = self
            .store
            .take_pending_postconditions(&context.runtime_scope)
        else {
            self.store.consume(&context.runtime_scope);
            return;
        };
        let run = run_postconditions(&pending.postconditions, pending.cwd.as_deref());
        let mut outcome = global_gate_outcome_store()
            .take(&context.runtime_scope)
            .unwrap_or_else(|| {
                GateOutcome::new(
                    &context.tool_name,
                    pending.tier,
                    "pass_gated",
                    GateReasonCode::RequirementsMet,
                    vec![],
                )
            });
        outcome.postcondition_passed = Some(run.passed);
        global_gate_outcome_store().record(&context.runtime_scope, outcome);
        if !run.passed {
            warn!(
                tool = %context.tool_name,
                assertion_failures = run.assertion_failures,
                env_errors = run.env_errors,
                "postcondition verification failed"
            );
        }
        self.store.consume(&context.runtime_scope);
    }
}

fn tool_meta_from_context(ctx: &ToolCallContext) -> ToolTierMeta {
    let access = ctx
        .metadata
        .get("access_level")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "delete" => baml_rt_tools::tools::ToolAccess::Delete,
            "write" => baml_rt_tools::tools::ToolAccess::Write,
            _ => baml_rt_tools::tools::ToolAccess::Read,
        })
        .unwrap_or(baml_rt_tools::tools::ToolAccess::Write);

    let tags = ctx
        .metadata
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    ToolTierMeta {
        access_level: access,
        tags,
        read_only_hint: None,
        destructive_hint: None,
        is_delegation: ctx.delegation_target.is_some(),
    }
}

/// Ingress trojan lint — inject warning into prompt context via block on LLM (audit path).
pub struct TrojanLintLLMInterceptor;

#[async_trait]
impl LLMInterceptor for TrojanLintLLMInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        let text = context.prompt.to_string();
        let found = trojan::detect(&text);
        if found.is_empty() {
            return Ok(InterceptorDecision::Allow);
        }
        warn!(phrases = ?found, "trojan phrases detected in prompt");
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<serde_json::Value>,
        _duration_ms: u64,
    ) {
    }
}
