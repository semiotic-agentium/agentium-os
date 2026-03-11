//! Embedding-based drift scoring utilities.
//!
//! This crate extracts intent text from prompts, extracts response text from
//! completed LLM results, computes embeddings, and classifies cosine-similarity
//! drift scores against configurable thresholds.

pub mod assessment;
pub mod config;
pub mod extraction;
pub mod provider;
pub mod similarity;

pub use assessment::{
    DEFAULT_TEXT_PREVIEW_CHARS, DriftAssessment, DriftSeverity, classify_score, preview_text,
    score_drift,
};
pub use config::{DriftConfig, DriftMode};
pub use provider::{EmbeddingProvider, FastEmbedProvider};
pub use similarity::cosine_similarity;
