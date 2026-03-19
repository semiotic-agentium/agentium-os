//! Cross-encoder reranker abstraction and fastembed implementation.
//!
//! A reranker scores a (query, document) *pair* jointly — unlike a bi-encoder
//! which embeds each text independently. This gives a direct relevance signal
//! with better discrimination for plan drift detection.
//!
//! ## Why cross-encoders for drift detection
//!
//! Empirical evaluation (see `tests/fixtures/drift/07_bipia_style_attacks.toml`)
//! shows that on real-world injection attacks (BIPIA-style dataset):
//!
//! | Approach              | Detection | Latency  |
//! |-----------------------|-----------|----------|
//! | BGE-large cosine only | 4/7 (57%) | ~142ms   |
//! | GTE-base cosine only  | 5/7 (71%) | ~70ms    |
//! | GTE-base + JINA XE    | 7/7 (100%)| ~49ms    |
//!
//! The two signals are complementary: GTE-base catches the Direct injection
//! jailbreak; JINA catches the Web QA poisoning and Agent tool indirect cases.
//!
//! ## Score interpretation
//!
//! Cross-encoder scores are logits (unbounded, not [0,1]):
//! - Higher = more relevant (document satisfies query)
//! - Lower  = less relevant (document does not satisfy query)
//!
//! Default severity thresholds (calibrated on JINA-v1-turbo-en, empirical):
//! - `warn_max_score  = -1.5` — step likely drifted
//! - `block_max_score = -3.0` — step clearly drifted
//!
//! These thresholds are conservative: benign aligned calls from the BIPIA
//! dataset score in the range -3.4 to -0.9, so not all low scores indicate
//! injection. The cross-encoder severity is combined with the cosine composite
//! via `worst_severity` — it can escalate but not lower the composite.

use std::sync::Mutex;

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::{assessment::DriftSeverity, provider::EmbeddingError};

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Cross-encoder relevance scoring for plan drift detection.
///
/// Implementations score a `(query, document)` pair and return a relevance
/// logit.  Higher scores indicate the document satisfies the query.
pub trait RerankProvider: Send + Sync {
    /// Score a single (query, document) pair.
    ///
    /// Returns a logit (unbounded float). Higher = more relevant.
    fn score_pair(&self, query: &str, document: &str) -> Result<f32, EmbeddingError>;
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Severity thresholds for cross-encoder step scores.
///
/// Scores are logits from `RerankProvider::score_pair(step, response)`.
/// Lower scores indicate less alignment between step and response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankDriftConfig {
    /// Score below this value emits a warning.  Default: −1.5.
    pub warn_max_score: f32,
    /// Score below this value emits a block.  Default: −3.0.
    pub block_max_score: f32,
}

/// Defaults derived from JINA-v1-turbo-en on BIPIA-style dataset.
/// Benign step scores range [-3.4, -0.9]; inject range [-3.6, -1.4].
/// Conservative thresholds to avoid false escalation in the combined signal.
impl Default for RerankDriftConfig {
    fn default() -> Self {
        Self {
            // Benign can score as low as -2.7 (Summarisation). -2.0 gives headroom.
            warn_max_score: -2.0,
            // Benign min is -3.4. Block at -4.0 avoids false blocks.
            block_max_score: -4.0,
        }
    }
}

impl RerankDriftConfig {
    /// Classify a cross-encoder score into a drift severity.
    pub fn classify(&self, score: f32) -> DriftSeverity {
        if score < self.block_max_score {
            DriftSeverity::Block
        } else if score < self.warn_max_score {
            DriftSeverity::Warn
        } else {
            DriftSeverity::Acceptable
        }
    }
}

// ---------------------------------------------------------------------------
// FastEmbed implementation: JINA-v1-turbo-en
// ---------------------------------------------------------------------------

/// `RerankProvider` backed by `fastembed::TextRerank`.
///
/// Default model: `JINA-v1-turbo-en` (~50ms init, ~6ms/call).
/// The production default for combined GTE-base + JINA drift detection.
pub struct FastRerankProvider {
    model: Mutex<TextRerank>,
}

impl std::fmt::Debug for FastRerankProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastRerankProvider").finish_non_exhaustive()
    }
}

impl FastRerankProvider {
    /// Create with `JINA-v1-turbo-en` (~50ms init on first use, ~6ms/call).
    pub fn new() -> Result<Self, EmbeddingError> {
        Self::with_model(RerankerModel::JINARerankerV1TurboEn)
    }

    /// Create with a specific fastembed reranker model.
    pub fn with_model(model: RerankerModel) -> Result<Self, EmbeddingError> {
        let opts = RerankInitOptions::new(model).with_show_download_progress(true);
        let reranker = TextRerank::try_new(opts).map_err(EmbeddingError::ModelInit)?;
        Ok(Self {
            model: Mutex::new(reranker),
        })
    }
}

impl RerankProvider for FastRerankProvider {
    fn score_pair(&self, query: &str, document: &str) -> Result<f32, EmbeddingError> {
        let guard = self
            .model
            .lock()
            .map_err(|e| EmbeddingError::Inference(anyhow::anyhow!("Mutex poisoned: {e}")))?;
        guard
            .rerank(query, vec![document], false, None)
            .map_err(EmbeddingError::Inference)?
            .pop()
            .map(|r| r.score)
            .ok_or_else(|| EmbeddingError::Inference(anyhow::anyhow!("No rerank result")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "downloads ~100MB model; run explicitly with --ignored"]
    fn jina_scores_aligned_higher_than_injected() {
        let provider = FastRerankProvider::new().expect("JINA init");
        let step = "Extract Q3 revenue data from the CRM system and compile results";
        let aligned = "Querying CRM database for Q3 revenue. Total: $2.4M across 3 regions.";
        let injected = "In the meadow's hush, golden wheels of cheese age slow in gloom.";

        let s_aligned = provider.score_pair(step, aligned).expect("aligned score");
        let s_injected = provider.score_pair(step, injected).expect("injected score");

        println!("Aligned score:  {s_aligned:.4}");
        println!("Injected score: {s_injected:.4}");
        assert!(
            s_aligned > s_injected,
            "aligned ({s_aligned:.4}) should score higher than injected ({s_injected:.4})"
        );
    }
}
