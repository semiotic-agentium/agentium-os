//! Per-task trajectory tracking for plan-anchored drift.
//!
//! [`TaskDriftTracker`] maintains a running exponentially-weighted centroid of
//! response embeddings and per-step alignment records.  It is created when an
//! intent is resolved and updated on each LLM completion within the task.

use serde::{Deserialize, Serialize};

use crate::similarity::cosine_similarity;

/// Per-step drift record accumulated during plan execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepDriftRecord {
    pub step_id: String,
    pub scores: Vec<f32>,
    pub mean_alignment: f32,
    pub min_alignment: f32,
}

impl StepDriftRecord {
    fn new(step_id: String) -> Self {
        Self {
            step_id,
            scores: Vec::new(),
            mean_alignment: 0.0,
            min_alignment: f32::MAX,
        }
    }

    fn record(&mut self, score: f32) {
        self.scores.push(score);
        self.min_alignment = self.min_alignment.min(score);
        let sum: f32 = self.scores.iter().sum();
        self.mean_alignment = sum / self.scores.len() as f32;
    }
}

/// Stateful tracker for a single task's drift trajectory.
///
/// The centroid is an exponentially-weighted moving average (EMA) of all
/// response embeddings observed within this task.  Comparing it against the
/// intent embedding reveals gradual trajectory creep that per-call scoring
/// cannot detect.
#[derive(Debug, Clone)]
pub struct TaskDriftTracker {
    intent_embedding: Vec<f32>,
    centroid: Vec<f32>,
    observation_count: u32,
    alpha: f32,
    step_records: Vec<StepDriftRecord>,
    current_step_id: Option<String>,
    is_revised: bool,
}

impl TaskDriftTracker {
    /// Create a new tracker anchored to the given intent embedding.
    ///
    /// `alpha` is the EMA decay factor (0.0–1.0).  Higher values weight recent
    /// observations more heavily.  A value of 0.15 means each new observation
    /// contributes ~15% to the centroid.
    pub fn new(intent_embedding: Vec<f32>, alpha: f32) -> Self {
        let centroid = intent_embedding.clone();
        Self {
            intent_embedding,
            centroid,
            observation_count: 0,
            alpha,
            step_records: Vec::new(),
            current_step_id: None,
            is_revised: false,
        }
    }

    /// Update the current step pointer (called on `PlanStepStatusChanged`
    /// when a step transitions to in-progress).
    pub fn set_current_step(&mut self, step_id: String) {
        if self.current_step_id.as_deref() != Some(&step_id) {
            if !self.step_records.iter().any(|r| r.step_id == step_id) {
                self.step_records
                    .push(StepDriftRecord::new(step_id.clone()));
            }
            self.current_step_id = Some(step_id);
        }
    }

    /// Mark the tracker as operating under a revised plan.
    pub fn mark_revised(&mut self) {
        self.is_revised = true;
    }

    /// Whether the plan has been revised.
    pub fn is_revised(&self) -> bool {
        self.is_revised
    }

    /// Replace the intent embedding (called when a plan is revised and the
    /// intent changes).
    pub fn reset_intent(&mut self, new_intent_embedding: Vec<f32>) {
        self.intent_embedding = new_intent_embedding;
        self.centroid = self.intent_embedding.clone();
        self.observation_count = 0;
    }

    /// Record a new response embedding, updating the EMA centroid and the
    /// current step's alignment record.
    ///
    /// Returns the cosine similarity between the updated centroid and the
    /// intent embedding (the trajectory drift score).
    pub fn observe(&mut self, response_embedding: &[f32], step_alignment: f32) -> f32 {
        self.observation_count += 1;

        // Update EMA centroid
        if self.centroid.len() == response_embedding.len() {
            for (c, r) in self.centroid.iter_mut().zip(response_embedding.iter()) {
                *c = self.alpha * r + (1.0 - self.alpha) * *c;
            }
        }

        // Record step-level alignment
        if let Some(ref step_id) = self.current_step_id
            && let Some(record) = self.step_records.iter_mut().find(|r| r.step_id == *step_id)
        {
            record.record(step_alignment);
        }

        cosine_similarity(&self.centroid, &self.intent_embedding)
    }

    pub fn observation_count(&self) -> u32 {
        self.observation_count
    }

    pub fn intent_embedding(&self) -> &[f32] {
        &self.intent_embedding
    }

    pub fn centroid(&self) -> &[f32] {
        &self.centroid
    }

    pub fn step_records(&self) -> &[StepDriftRecord] {
        &self.step_records
    }

    /// Trajectory drift: cosine similarity between current centroid and the
    /// intent embedding.  Returns 1.0 if no observations have been made.
    pub fn trajectory_drift_score(&self) -> f32 {
        if self.observation_count == 0 {
            return 1.0;
        }
        cosine_similarity(&self.centroid, &self.intent_embedding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_starts_with_centroid_equal_to_intent() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let tracker = TaskDriftTracker::new(intent.clone(), 0.15);
        assert_eq!(tracker.centroid(), &intent);
        assert_eq!(tracker.observation_count(), 0);
        let score = tracker.trajectory_drift_score();
        assert!(
            (score - 1.0).abs() < 1e-6,
            "initial trajectory should be 1.0, got {score}"
        );
    }

    #[test]
    fn observe_updates_centroid_via_ema() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let mut tracker = TaskDriftTracker::new(intent, 0.5);

        // Response orthogonal to intent
        let response = vec![0.0, 1.0, 0.0, 0.0];
        tracker.set_current_step("step-1".into());
        let traj = tracker.observe(&response, 0.0);

        // Centroid should now be midway between intent and response
        // (alpha=0.5 so equal weight).  Trajectory drift should be < 1.0.
        assert!(traj < 1.0, "trajectory should decrease, got {traj}");
        assert_eq!(tracker.observation_count(), 1);

        // Centroid: 0.5*[0,1,0,0] + 0.5*[1,0,0,0] = [0.5, 0.5, 0, 0]
        let c = tracker.centroid();
        assert!((c[0] - 0.5).abs() < 1e-6);
        assert!((c[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn aligned_observations_keep_centroid_near_intent() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let mut tracker = TaskDriftTracker::new(intent, 0.15);

        tracker.set_current_step("step-1".into());
        for _ in 0..10 {
            let response = vec![0.95, 0.05, 0.0, 0.0];
            tracker.observe(&response, 0.95);
        }

        let traj = tracker.trajectory_drift_score();
        assert!(traj > 0.95, "centroid should stay near intent, got {traj}");
    }

    #[test]
    fn divergent_observations_pull_centroid_away() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let mut tracker = TaskDriftTracker::new(intent, 0.3);

        tracker.set_current_step("step-1".into());
        for _ in 0..20 {
            let response = vec![0.0, 0.0, 1.0, 0.0];
            tracker.observe(&response, 0.1);
        }

        let traj = tracker.trajectory_drift_score();
        assert!(
            traj < 0.5,
            "centroid should drift far from intent, got {traj}"
        );
    }

    #[test]
    fn step_records_accumulate_scores() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let mut tracker = TaskDriftTracker::new(intent, 0.15);

        tracker.set_current_step("step-a".into());
        tracker.observe(&[0.9, 0.1, 0.0, 0.0], 0.9);
        tracker.observe(&[0.8, 0.2, 0.0, 0.0], 0.8);

        tracker.set_current_step("step-b".into());
        tracker.observe(&[0.5, 0.5, 0.0, 0.0], 0.5);

        let records = tracker.step_records();
        assert_eq!(records.len(), 2);

        assert_eq!(records[0].step_id, "step-a");
        assert_eq!(records[0].scores.len(), 2);
        assert!((records[0].mean_alignment - 0.85).abs() < 1e-6);
        assert!((records[0].min_alignment - 0.8).abs() < 1e-6);

        assert_eq!(records[1].step_id, "step-b");
        assert_eq!(records[1].scores.len(), 1);
        assert!((records[1].mean_alignment - 0.5).abs() < 1e-6);
    }

    #[test]
    fn reset_intent_resets_centroid_and_count() {
        let intent = vec![1.0, 0.0, 0.0, 0.0];
        let mut tracker = TaskDriftTracker::new(intent, 0.15);

        tracker.set_current_step("step-1".into());
        tracker.observe(&[0.0, 1.0, 0.0, 0.0], 0.0);
        assert_eq!(tracker.observation_count(), 1);
        assert!(tracker.trajectory_drift_score() < 1.0);

        let new_intent = vec![0.0, 1.0, 0.0, 0.0];
        tracker.reset_intent(new_intent.clone());
        assert_eq!(tracker.observation_count(), 0);
        assert!((tracker.trajectory_drift_score() - 1.0).abs() < 1e-6);
        assert_eq!(tracker.intent_embedding(), &new_intent);
    }

    #[test]
    fn mark_revised_persists() {
        let tracker_a = TaskDriftTracker::new(vec![1.0; 4], 0.15);
        assert!(!tracker_a.is_revised());

        let mut tracker_b = TaskDriftTracker::new(vec![1.0; 4], 0.15);
        tracker_b.mark_revised();
        assert!(tracker_b.is_revised());
    }
}
