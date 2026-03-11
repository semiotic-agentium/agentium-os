//! Embedding-based drift detection for prompt injection defense.
//!
//! This crate provides an [`LLMInterceptor`] implementation that detects when
//! an LLM response deviates from the coordinator's intended prompt — a signal
//! of prompt injection via untrusted data in the conversation context.
//!
//! # Architecture
//!
//! 1. On `intercept_llm_call`, the **intent** (last user message minus untrusted
//!    data blocks) is embedded and stashed.
//! 2. On `on_llm_call_complete`, the **response** is embedded and compared to
//!    the stashed intent via cosine similarity.
//! 3. If similarity drops below configurable thresholds the event is logged
//!    (audit mode) or the *next* LLM call in the same ReAct loop is blocked
//!    (enforce mode).

pub mod config;
pub mod drift;
pub mod extraction;
pub mod provider;
pub mod similarity;

pub use config::{DriftConfig, DriftMode};
pub use drift::DriftDetectorInterceptor;
pub use provider::{EmbeddingProvider, FastEmbedProvider};
pub use similarity::cosine_similarity;
