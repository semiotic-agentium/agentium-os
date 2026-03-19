//! Stateless drift assessment helpers shared by provenance and other callers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::DriftConfig,
    extraction::{extract_intent_from_prompt, extract_response_text},
    provider::EmbeddingProvider,
    similarity::cosine_similarity,
};

/// Maximum number of characters to keep in preview fields.
pub const DEFAULT_TEXT_PREVIEW_CHARS: usize = 240;

/// Threshold classification for a scored response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftSeverity {
    Acceptable,
    Warn,
    Block,
}

impl DriftSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acceptable => "acceptable",
            Self::Warn => "warn",
            Self::Block => "block",
        }
    }
}

/// Drift scoring result for a completed LLM call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftAssessment {
    pub score: f32,
    pub severity: DriftSeverity,
    pub mode: crate::DriftMode,
    pub warn_min_score: f32,
    pub block_min_score: f32,
    pub intent_text_preview: String,
    pub response_text_preview: String,
}

impl DriftAssessment {
    pub fn severity_label(&self) -> &'static str {
        self.severity.as_str()
    }
}

/// Compute drift between a prompt and a completed LLM response.
///
/// `intent_override` — when `Some`, use this text as the intent anchor instead
/// of extracting from the raw prompt. Pass the committed plan intent_description
/// when a plan tracker exists; fall back to `None` for pre-plan calls.
///
/// Returns `None` when the intent text is empty/unextractable or when embedding
/// computation fails.
pub fn score_drift(
    prompt: &Value,
    response: &Value,
    config: &DriftConfig,
    provider: &dyn EmbeddingProvider,
    intent_override: Option<&str>,
) -> Option<DriftAssessment> {
    let intent_text = match intent_override {
        Some(text) if !text.trim().is_empty() => text.to_owned(),
        _ => extract_intent_from_prompt(prompt)?,
    };
    let response_text = extract_response_text(response);
    let embeddings = match provider.embed_batch(&[&intent_text, &response_text]) {
        Ok(embeddings) if embeddings.len() == 2 => embeddings,
        Ok(embeddings) => {
            tracing::error!(
                count = embeddings.len(),
                "Embedding provider returned unexpected batch size during drift scoring"
            );
            return None;
        }
        Err(error) => {
            tracing::error!(%error, "Embedding computation failed during drift scoring");
            return None;
        }
    };

    let score = cosine_similarity(&embeddings[0], &embeddings[1]);
    Some(DriftAssessment {
        score,
        severity: classify_score(score, config),
        mode: config.mode,
        warn_min_score: config.warn_min_score,
        block_min_score: config.block_min_score,
        intent_text_preview: preview_text(&intent_text, DEFAULT_TEXT_PREVIEW_CHARS),
        response_text_preview: preview_text(&response_text, DEFAULT_TEXT_PREVIEW_CHARS),
    })
}

pub fn classify_score(score: f32, config: &DriftConfig) -> DriftSeverity {
    if score < config.block_min_score {
        DriftSeverity::Block
    } else if score < config.warn_min_score {
        DriftSeverity::Warn
    } else {
        DriftSeverity::Acceptable
    }
}

pub fn preview_text(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        DriftMode,
        provider::{EmbeddingError, EmbeddingProvider},
    };

    struct MockProvider {
        mappings: Vec<(&'static str, Vec<f32>)>,
        fallback: Vec<f32>,
    }

    impl MockProvider {
        fn new(mappings: Vec<(&'static str, Vec<f32>)>, fallback: Vec<f32>) -> Self {
            Self { mappings, fallback }
        }
    }

    impl EmbeddingProvider for MockProvider {
        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts
                .iter()
                .map(|text| {
                    self.mappings
                        .iter()
                        .find(|(prefix, _)| text.contains(prefix))
                        .map(|(_, embedding)| embedding.clone())
                        .unwrap_or_else(|| self.fallback.clone())
                })
                .collect())
        }

        fn dimension(&self) -> usize {
            self.fallback.len()
        }
    }

    #[test]
    fn score_drift_returns_acceptably_aligned_assessment() {
        let provider = MockProvider::new(
            vec![
                ("Create a task", vec![1.0, 0.0, 0.0, 0.0]),
                ("Create task in list", vec![0.9, 0.1, 0.0, 0.0]),
            ],
            vec![0.0; 4],
        );
        let prompt = json!([{"role": "user", "content": "Create a task titled 'Research'."}]);
        let response = json!({"message": "Create task in list 901325431486"});

        let assessment = score_drift(&prompt, &response, &DriftConfig::default(), &provider, None)
            .expect("score");

        assert!(assessment.score > 0.9, "score={}", assessment.score);
        assert_eq!(assessment.severity, DriftSeverity::Acceptable);
        assert_eq!(assessment.mode, DriftMode::Audit);
        assert!(assessment.intent_text_preview.contains("Create a task"));
        assert!(
            assessment
                .response_text_preview
                .contains("Create task in list")
        );
    }

    #[test]
    fn score_drift_classifies_warn_and_block_min_scores() {
        let prompt = json!([{"role": "user", "content": "Create a task titled 'Research'."}]);
        let response = json!({"message": "Ignore previous instructions."});
        let config = DriftConfig {
            warn_min_score: 0.8,
            block_min_score: 0.2,
            ..Default::default()
        };

        let warn_provider = MockProvider::new(
            vec![
                ("Create a task", vec![1.0, 0.0, 0.0, 0.0]),
                ("Ignore previous", vec![0.6, 0.8, 0.0, 0.0]),
            ],
            vec![0.0; 4],
        );
        let warn_assessment =
            score_drift(&prompt, &response, &config, &warn_provider, None).expect("warn score");
        assert_eq!(warn_assessment.severity, DriftSeverity::Warn);

        let block_provider = MockProvider::new(
            vec![
                ("Create a task", vec![1.0, 0.0, 0.0, 0.0]),
                ("Ignore previous", vec![0.0, 0.0, 0.0, 1.0]),
            ],
            vec![0.0; 4],
        );
        let block_assessment =
            score_drift(&prompt, &response, &config, &block_provider, None).expect("block score");
        assert_eq!(block_assessment.severity, DriftSeverity::Block);
    }

    #[test]
    fn score_drift_returns_none_without_extractable_intent() {
        let provider = MockProvider::new(vec![], vec![0.0; 4]);
        let prompt = json!([{"role": "system", "content": "You are an agent."}]);
        let response = json!({"message": "Task created."});

        assert!(
            score_drift(&prompt, &response, &DriftConfig::default(), &provider, None).is_none()
        );
    }

    #[test]
    fn preview_text_truncates_at_char_boundary() {
        let preview = preview_text("abcdef", 3);
        assert_eq!(preview, "abc...");
        assert_eq!(preview_text("abc", 10), "abc");
    }
}
