//! Embedding-based drift scoring utilities.
//!
//! This crate extracts intent text from prompts, extracts response text from
//! completed LLM results, computes embeddings, and classifies cosine-similarity
//! drift scores against configurable thresholds.
//!
//! ## Plan-anchored drift
//!
//! When a task has a committed plan, the [`plan_assessment`] module scores LLM
//! responses against the intent description and current step description in
//! addition to the tactical prompt-vs-response drift.  The [`trajectory`]
//! module tracks an EMA centroid per task to detect gradual trajectory creep.

pub mod assessment;
pub mod config;
pub mod cross_encoder_validation;
#[cfg(test)]
mod drift_fixture_tests;
pub mod extraction;
pub mod plan_assessment;
pub mod plan_context;
pub mod provider;
pub mod reranker;
pub mod similarity;
pub mod trajectory;

pub use assessment::{
    DEFAULT_TEXT_PREVIEW_CHARS, DriftAssessment, DriftSeverity, classify_score, preview_text,
    score_drift,
};
pub use config::{DriftConfig, DriftMode};
pub use plan_assessment::{
    PlanDriftAssessment, PlanDriftConfig, PlanDriftInputs, score_plan_drift,
};
pub use plan_context::{PlanDriftContext, PlanStepAnchor};
pub use provider::{EmbeddingProvider, FastEmbedProvider};
pub use reranker::{FastRerankProvider, RerankDriftConfig, RerankProvider};
pub use similarity::cosine_similarity;
pub use trajectory::{StepDriftRecord, TaskDriftTracker};
