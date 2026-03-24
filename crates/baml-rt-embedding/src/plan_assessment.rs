//! Plan-anchored drift assessment: scores an LLM response against the
//! committed plan (intent, current step, trajectory centroid) in addition to
//! the existing tactical prompt-vs-response drift.

use serde::{Deserialize, Serialize};

use crate::{
    DriftSeverity, assessment::DriftAssessment, reranker::RerankDriftConfig,
    similarity::cosine_similarity, trajectory::TaskDriftTracker,
};

/// Configuration for plan-anchored drift thresholds.
///
/// Separate from [`crate::DriftConfig`] because the anchor semantics differ:
/// tactical drift compares prompt-to-response, while plan drift compares
/// strategic artifacts (intent, step description) to response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDriftConfig {
    /// Intent-vs-response similarity below this emits a warning.
    pub intent_warn_min: f32,
    /// Intent-vs-response similarity below this triggers a block.
    pub intent_block_min: f32,

    /// Step-vs-response similarity below this emits a warning.
    pub step_warn_min: f32,
    /// Step-vs-response similarity below this triggers a block.
    pub step_block_min: f32,

    /// Trajectory centroid-vs-intent similarity below this emits a warning.
    pub trajectory_warn_min: f32,
    /// Trajectory centroid-vs-intent similarity below this triggers a block.
    pub trajectory_block_min: f32,

    /// EMA decay factor for the trajectory centroid (0.0–1.0).
    /// Higher = more weight on recent observations.
    pub ema_alpha: f32,

    /// Multiplicative weight boost for early plan steps (per WebAnchor).
    /// Applied to steps where `order < total_steps / 2`.
    pub early_step_weight: f32,

    /// Threshold relaxation applied when `PlanDriftContext::is_revised_plan`
    /// is true.  Subtracted from warn/block thresholds to accommodate
    /// legitimate trajectory discontinuities after plan revision.
    pub revision_leniency: f32,

    /// Thresholds for classifying cross-encoder step scores.
    /// The reranker is always active in `PlanCommitted` scoring.
    #[serde(default)]
    pub rerank: RerankDriftConfig,
}

/// Defaults derived from empirical evaluation on BIPIA-style injection attack
/// dataset (7 categories) and synthetic CRM-vs-poetry contrast pairs in embedding tests.
/// See `docs/drift-catalogue.md` "Threshold Calibration Guide" for the data.
impl Default for PlanDriftConfig {
    fn default() -> Self {
        Self {
            // GTE-base benign min: 0.556.  0.50 gives 0.056 headroom.
            intent_warn_min: 0.50,
            intent_block_min: 0.20,
            // Step descriptions are more specific → higher aligned scores.
            step_warn_min: 0.45,
            step_block_min: 0.20,
            // Centroid is slow-moving (stays >0.83 after 3 injections).
            trajectory_warn_min: 0.55,
            trajectory_block_min: 0.30,
            ema_alpha: 0.15,
            early_step_weight: 1.5,
            revision_leniency: 0.10,
            rerank: RerankDriftConfig::default(),
        }
    }
}

impl PlanDriftConfig {
    fn effective_threshold(&self, base: f32, is_revised: bool) -> f32 {
        if is_revised {
            (base - self.revision_leniency).max(0.0)
        } else {
            base
        }
    }

    fn classify_intent(&self, score: f32, is_revised: bool) -> DriftSeverity {
        classify(
            score,
            self.effective_threshold(self.intent_warn_min, is_revised),
            self.effective_threshold(self.intent_block_min, is_revised),
        )
    }

    fn classify_step(&self, score: f32, is_revised: bool) -> DriftSeverity {
        classify(
            score,
            self.effective_threshold(self.step_warn_min, is_revised),
            self.effective_threshold(self.step_block_min, is_revised),
        )
    }

    fn classify_trajectory(&self, score: f32, is_revised: bool) -> DriftSeverity {
        classify(
            score,
            self.effective_threshold(self.trajectory_warn_min, is_revised),
            self.effective_threshold(self.trajectory_block_min, is_revised),
        )
    }
}

fn classify(score: f32, warn_min: f32, block_min: f32) -> DriftSeverity {
    if score < block_min {
        DriftSeverity::Block
    } else if score < warn_min {
        DriftSeverity::Warn
    } else {
        DriftSeverity::Acceptable
    }
}

/// Plan-anchored drift assessment — discriminated by plan phase.
///
/// Pre-plan assessments have no step data (structurally impossible to
/// produce step alignment). Post-plan assessments always have step
/// alignment (structurally guaranteed by the committed plan's `NonEmptySteps`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase")]
pub enum PlanDriftAssessment {
    /// Assessment for LLM calls before a plan is committed.
    /// Only intent alignment and trajectory drift are scored.
    #[serde(rename = "pre_plan")]
    PrePlan {
        tactical: DriftAssessment,
        intent_alignment: f32,
        trajectory_drift: f32,
        plan_adherence_score: f32,
        composite_severity: DriftSeverity,
    },

    /// Assessment for LLM calls after a plan is committed.
    /// Step alignment is always present — guaranteed by the type system.
    /// `cross_encoder_step_score` is present when a `RerankProvider` is
    /// configured and provides a complementary pairwise signal.
    #[serde(rename = "plan_committed")]
    PlanCommitted {
        tactical: DriftAssessment,
        intent_alignment: f32,
        step_alignment: f32,
        trajectory_drift: f32,
        plan_adherence_score: f32,
        /// Cross-encoder relevance logit for (step_description, response).
        /// Logit scale (unbounded): higher = more relevant.
        /// The reranker is always present in committed-plan scoring.
        cross_encoder_step_score: f32,
        composite_severity: DriftSeverity,
    },
}

impl PlanDriftAssessment {
    pub fn intent_alignment(&self) -> f32 {
        match self {
            Self::PrePlan {
                intent_alignment, ..
            } => *intent_alignment,
            Self::PlanCommitted {
                intent_alignment, ..
            } => *intent_alignment,
        }
    }

    pub fn step_alignment(&self) -> Option<f32> {
        match self {
            Self::PrePlan { .. } => None,
            Self::PlanCommitted { step_alignment, .. } => Some(*step_alignment),
        }
    }

    pub fn trajectory_drift(&self) -> f32 {
        match self {
            Self::PrePlan {
                trajectory_drift, ..
            } => *trajectory_drift,
            Self::PlanCommitted {
                trajectory_drift, ..
            } => *trajectory_drift,
        }
    }

    pub fn plan_adherence_score(&self) -> f32 {
        match self {
            Self::PrePlan {
                plan_adherence_score,
                ..
            } => *plan_adherence_score,
            Self::PlanCommitted {
                plan_adherence_score,
                ..
            } => *plan_adherence_score,
        }
    }

    pub fn composite_severity(&self) -> DriftSeverity {
        match self {
            Self::PrePlan {
                composite_severity, ..
            } => *composite_severity,
            Self::PlanCommitted {
                composite_severity, ..
            } => *composite_severity,
        }
    }
}

/// Inputs for plan drift scoring — discriminated by plan phase.
///
/// `PrePlan`: only response embedding. Step alignment is structurally absent.
/// `WithStep`: step embedding is always present (guaranteed by caller's
/// `CommittedPlanExecution`).
pub enum PlanDriftInputs<'a> {
    PrePlan {
        response_embedding: &'a [f32],
        is_revised: bool,
    },
    WithStep {
        step_embedding: &'a [f32],
        response_embedding: &'a [f32],
        step_index: u32,
        total_steps: u32,
        is_revised: bool,
        /// Cross-encoder relevance logit for (step_description, response).
        /// Always present in PlanCommitted scoring — the reranker is always
        /// configured. Logit scale: higher = more relevant.
        cross_encoder_step_score: f32,
    },
}

/// Score plan-anchored drift. Dispatches on the input variant to produce the
/// matching assessment variant. No `Option` step scores, no phantom zeros.
pub fn score_plan_drift(
    inputs: &PlanDriftInputs<'_>,
    tactical: DriftAssessment,
    tracker: &mut TaskDriftTracker,
    config: &PlanDriftConfig,
) -> PlanDriftAssessment {
    let intent_alignment =
        cosine_similarity(tracker.intent_embedding(), inputs.response_embedding());

    match inputs {
        PlanDriftInputs::PrePlan {
            response_embedding,
            is_revised,
        } => {
            let trajectory_drift = tracker.observe(response_embedding, 0.0);
            let plan_adherence_score = intent_alignment.clamp(0.0, 1.0);
            let intent_sev = config.classify_intent(intent_alignment, *is_revised);
            let trajectory_sev = config.classify_trajectory(trajectory_drift, *is_revised);
            let adherence_sev = classify(
                plan_adherence_score,
                config.effective_threshold(config.step_warn_min, *is_revised),
                config.effective_threshold(config.step_block_min, *is_revised),
            );
            let composite_severity = worst_severity(&[intent_sev, trajectory_sev, adherence_sev]);

            PlanDriftAssessment::PrePlan {
                tactical,
                intent_alignment,
                trajectory_drift,
                plan_adherence_score,
                composite_severity,
            }
        }
        PlanDriftInputs::WithStep {
            step_embedding,
            response_embedding,
            step_index,
            total_steps,
            is_revised,
            cross_encoder_step_score,
        } => {
            let step_alignment = cosine_similarity(step_embedding, response_embedding);
            let trajectory_drift = tracker.observe(response_embedding, step_alignment);

            let is_early = *total_steps > 0 && *step_index < *total_steps / 2;
            let step_weight = if is_early {
                config.early_step_weight
            } else {
                1.0
            };
            // For PlanCommitted: the step IS the operative anchor. Intent is
            // informational context — it contributes minimally so it doesn't
            // penalise legitimate tool actions that are semantically close to
            // their assigned step but distant from the broad user goal.
            let plan_adherence_score =
                (0.1 * intent_alignment + 0.9 * step_alignment * step_weight).clamp(0.0, 1.0);

            let _intent_sev = config.classify_intent(intent_alignment, *is_revised);
            let step_sev = config.classify_step(step_alignment, *is_revised);
            let trajectory_sev = config.classify_trajectory(trajectory_drift, *is_revised);
            let adherence_sev = classify(
                plan_adherence_score,
                config.effective_threshold(config.step_warn_min, *is_revised),
                config.effective_threshold(config.step_block_min, *is_revised),
            );

            // XE signal always present in PlanCommitted. Incorporated as an
            // additional severity dimension — can escalate but not reduce.
            let xe_sev = config.rerank.classify(*cross_encoder_step_score);
            // For PlanCommitted: the step is the operative anchor, not the
            // intent. Intent contributes to adherence but should not
            // independently trigger warn/block — a tool action matching its
            // assigned step is correct even if semantically distant from the
            // broad user goal.
            let composite_severity =
                worst_severity(&[step_sev, trajectory_sev, adherence_sev, xe_sev]);

            PlanDriftAssessment::PlanCommitted {
                tactical,
                intent_alignment,
                step_alignment,
                trajectory_drift,
                plan_adherence_score,
                cross_encoder_step_score: *cross_encoder_step_score,
                composite_severity,
            }
        }
    }
}

impl PlanDriftInputs<'_> {
    fn response_embedding(&self) -> &[f32] {
        match self {
            Self::PrePlan {
                response_embedding, ..
            } => response_embedding,
            Self::WithStep {
                response_embedding, ..
            } => response_embedding,
        }
    }
}

fn worst_severity(severities: &[DriftSeverity]) -> DriftSeverity {
    if severities.contains(&DriftSeverity::Block) {
        DriftSeverity::Block
    } else if severities.contains(&DriftSeverity::Warn) {
        DriftSeverity::Warn
    } else {
        DriftSeverity::Acceptable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DriftMode;

    fn mock_tactical(score: f32) -> DriftAssessment {
        DriftAssessment {
            score,
            severity: if score < 0.25 {
                DriftSeverity::Block
            } else if score < 0.5 {
                DriftSeverity::Warn
            } else {
                DriftSeverity::Acceptable
            },
            mode: DriftMode::Audit,
            warn_min_score: 0.5,
            block_min_score: 0.25,
            intent_text_preview: "test intent".into(),
            response_text_preview: "test response".into(),
        }
    }

    fn with_step<'a>(
        step: &'a [f32],
        response: &'a [f32],
        si: u32,
        ts: u32,
        rev: bool,
    ) -> PlanDriftInputs<'a> {
        PlanDriftInputs::WithStep {
            step_embedding: step,
            response_embedding: response,
            step_index: si,
            total_steps: ts,
            is_revised: rev,
            // Tests use 0.0 as a neutral XE score (well above block threshold -3.0)
            // so the XE dimension doesn't affect existing test assertions.
            cross_encoder_step_score: 0.0,
        }
    }

    fn pre_plan(response: &[f32], rev: bool) -> PlanDriftInputs<'_> {
        PlanDriftInputs::PrePlan {
            response_embedding: response,
            is_revised: rev,
        }
    }

    #[test]
    fn aligned_execution_produces_acceptable_composite() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let step_emb = vec![0.95, 0.05, 0.0, 0.0];
        let response_emb = vec![0.9, 0.1, 0.0, 0.0];
        let config = PlanDriftConfig::default();
        let mut tracker = TaskDriftTracker::new(intent_emb, config.ema_alpha);
        tracker.set_current_step("step-0".into());

        let result = score_plan_drift(
            &with_step(&step_emb, &response_emb, 0, 3, false),
            mock_tactical(0.9),
            &mut tracker,
            &config,
        );

        assert!(
            result.intent_alignment() > 0.9,
            "got {}",
            result.intent_alignment()
        );
        assert!(
            result.step_alignment().unwrap() > 0.9,
            "got {:?}",
            result.step_alignment()
        );
        assert!(
            result.trajectory_drift() > 0.9,
            "got {}",
            result.trajectory_drift()
        );
        assert_eq!(result.composite_severity(), DriftSeverity::Acceptable);
    }

    #[test]
    fn injection_produces_block_composite() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let step_emb = vec![0.95, 0.05, 0.0, 0.0];
        let response_emb = vec![0.0, 0.0, 0.0, 1.0];
        let config = PlanDriftConfig::default();
        let mut tracker = TaskDriftTracker::new(intent_emb, config.ema_alpha);
        tracker.set_current_step("step-0".into());

        let result = score_plan_drift(
            &with_step(&step_emb, &response_emb, 0, 3, false),
            mock_tactical(0.1),
            &mut tracker,
            &config,
        );

        assert!(
            result.intent_alignment() < 0.15,
            "got {}",
            result.intent_alignment()
        );
        assert!(
            result.step_alignment().unwrap() < 0.15,
            "got {:?}",
            result.step_alignment()
        );
        assert_eq!(result.composite_severity(), DriftSeverity::Block);
    }

    #[test]
    fn revision_leniency_relaxes_thresholds() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let step_emb = vec![0.7, 0.7, 0.0, 0.0];
        let response_emb = vec![0.6, 0.8, 0.0, 0.0];
        let config = PlanDriftConfig::default();

        let mut tracker_no_rev = TaskDriftTracker::new(intent_emb.clone(), config.ema_alpha);
        tracker_no_rev.set_current_step("step-1".into());
        let result_no_rev = score_plan_drift(
            &with_step(&step_emb, &response_emb, 1, 4, false),
            mock_tactical(0.6),
            &mut tracker_no_rev,
            &config,
        );

        let mut tracker_rev = TaskDriftTracker::new(intent_emb, config.ema_alpha);
        tracker_rev.set_current_step("step-1".into());
        tracker_rev.mark_revised();
        let result_rev = score_plan_drift(
            &with_step(&step_emb, &response_emb, 1, 4, true),
            mock_tactical(0.6),
            &mut tracker_rev,
            &config,
        );

        assert!((result_no_rev.intent_alignment() - result_rev.intent_alignment()).abs() < 1e-6);
        assert!(
            severity_ord(result_rev.composite_severity())
                <= severity_ord(result_no_rev.composite_severity()),
        );
    }

    #[test]
    fn early_step_weight_boosts_adherence() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let step_emb = vec![0.8, 0.2, 0.0, 0.0];
        let response_emb = vec![0.8, 0.2, 0.0, 0.0];
        let config = PlanDriftConfig::default();

        let mut tracker_early = TaskDriftTracker::new(intent_emb.clone(), config.ema_alpha);
        tracker_early.set_current_step("step-0".into());
        let early = score_plan_drift(
            &with_step(&step_emb, &response_emb, 0, 4, false),
            mock_tactical(0.8),
            &mut tracker_early,
            &config,
        );

        let mut tracker_late = TaskDriftTracker::new(intent_emb, config.ema_alpha);
        tracker_late.set_current_step("step-3".into());
        let late = score_plan_drift(
            &with_step(&step_emb, &response_emb, 3, 4, false),
            mock_tactical(0.8),
            &mut tracker_late,
            &config,
        );

        assert!(
            early.plan_adherence_score() >= late.plan_adherence_score(),
            "early ({}) >= late ({})",
            early.plan_adherence_score(),
            late.plan_adherence_score(),
        );
    }

    #[test]
    fn pre_plan_call_has_no_step_alignment_and_is_not_block() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let response_emb = vec![0.9, 0.1, 0.0, 0.0];
        let config = PlanDriftConfig::default();
        let mut tracker = TaskDriftTracker::new(intent_emb, config.ema_alpha);

        let result = score_plan_drift(
            &pre_plan(&response_emb, false),
            mock_tactical(0.9),
            &mut tracker,
            &config,
        );

        assert!(
            matches!(result, PlanDriftAssessment::PrePlan { .. }),
            "pre-plan call should produce PrePlan variant"
        );
        assert!(result.step_alignment().is_none());
        assert_ne!(result.composite_severity(), DriftSeverity::Block);
        assert!(result.intent_alignment() > 0.9);
    }

    #[test]
    fn post_plan_call_always_has_step_alignment() {
        let intent_emb = vec![1.0, 0.0, 0.0, 0.0];
        let step_emb = vec![0.8, 0.2, 0.0, 0.0];
        let response_emb = vec![0.8, 0.2, 0.0, 0.0];
        let config = PlanDriftConfig::default();
        let mut tracker = TaskDriftTracker::new(intent_emb, config.ema_alpha);
        tracker.set_current_step("step-0".into());

        let result = score_plan_drift(
            &with_step(&step_emb, &response_emb, 0, 2, false),
            mock_tactical(0.8),
            &mut tracker,
            &config,
        );

        assert!(
            matches!(result, PlanDriftAssessment::PlanCommitted { .. }),
            "post-plan call should produce PlanCommitted variant"
        );
        assert!(result.step_alignment().is_some());
    }

    fn severity_ord(s: DriftSeverity) -> u8 {
        match s {
            DriftSeverity::Acceptable => 0,
            DriftSeverity::Warn => 1,
            DriftSeverity::Block => 2,
        }
    }
}
