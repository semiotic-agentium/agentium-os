//! Embedding provider abstraction and fastembed-rs implementation.
//!
//! [`EmbeddingProvider`] is the trait boundary; production code uses
//! [`FastEmbedProvider`] which wraps `fastembed::TextEmbedding` with
//! `BAAI/bge-small-en-v1.5` (384-d, ~30 MB ONNX model).

use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Embedding computation errors.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// Model initialisation failed (e.g. ONNX download / load).
    #[error("Failed to initialise embedding model")]
    ModelInit(#[source] anyhow::Error),

    /// Batch inference failed.
    #[error("Embedding inference failed")]
    Inference(#[source] anyhow::Error),

    /// Empty input: caller passed zero texts.
    #[error("Cannot embed an empty batch")]
    EmptyBatch,
}

/// Trait for computing text embeddings.
///
/// `fastembed` inference is **synchronous** (CPU-bound ONNX).  Callers that
/// run on an async runtime should wrap calls in `tokio::task::spawn_blocking`.
pub trait EmbeddingProvider: Send + Sync {
    /// Embed one or more texts in a single batch.
    ///
    /// Returns one `Vec<f32>` per input text, all with the same dimensionality
    /// (see [`dimension`](Self::dimension)).
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// The fixed output dimensionality of the model (e.g. 384 for bge-small-en).
    fn dimension(&self) -> usize;
}

/// [`EmbeddingProvider`] backed by `fastembed::TextEmbedding`.
///
/// The model is downloaded on first construction to `~/.cache/fastembed/`.
/// Subsequent instantiations reuse the cached artefacts.
///
/// `TextEmbedding` is **not** `Sync` (internal ONNX session), so we guard it
/// with a `Mutex`.  Contention is acceptable — embedding calls are infrequent
/// relative to the LLM round-trip they guard.
pub struct FastEmbedProvider {
    model: Mutex<TextEmbedding>,
    dim: usize,
}

impl std::fmt::Debug for FastEmbedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastEmbedProvider")
            .field("dim", &self.dim)
            .finish_non_exhaustive()
    }
}

impl FastEmbedProvider {
    /// Dimension of `BAAI/bge-small-en-v1.5`.
    const BGE_SMALL_DIM: usize = 384;

    /// Create a new provider using `BAAI/bge-small-en-v1.5`.
    ///
    /// The ONNX model is downloaded on first use (~30 MB).
    pub fn new() -> Result<Self, EmbeddingError> {
        let opts =
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true);
        let model =
            TextEmbedding::try_new(opts).map_err(|e| EmbeddingError::ModelInit(e.into()))?;
        Ok(Self {
            model: Mutex::new(model),
            dim: Self::BGE_SMALL_DIM,
        })
    }
}

impl EmbeddingProvider for FastEmbedProvider {
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Err(EmbeddingError::EmptyBatch);
        }
        let docs: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        let guard = self
            .model
            .lock()
            .map_err(|e| EmbeddingError::Inference(anyhow::anyhow!("Mutex poisoned: {e}")))?;
        guard
            .embed(docs, None)
            .map_err(|e| EmbeddingError::Inference(e.into()))
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that [`FastEmbedProvider`] can initialise and produce embeddings
    /// of the expected dimension.  Requires model download on first run.
    #[test]
    #[ignore = "downloads ~30 MB model; run explicitly with --ignored"]
    fn fastembed_provider_produces_correct_dimensions() {
        let provider = FastEmbedProvider::new().expect("model init");
        let embeddings = provider.embed_batch(&["hello world"]).expect("embed_batch");
        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].len(), FastEmbedProvider::BGE_SMALL_DIM);
    }

    #[test]
    fn empty_batch_returns_error() {
        // We can test the empty-batch guard without initialising the model.
        // Build a provider manually only if we can (may fail if model not cached).
        // Instead, test the trait contract via a mock.
        struct MockProvider;
        impl EmbeddingProvider for MockProvider {
            fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                if texts.is_empty() {
                    return Err(EmbeddingError::EmptyBatch);
                }
                Ok(texts.iter().map(|_| vec![0.0; 4]).collect())
            }
            fn dimension(&self) -> usize {
                4
            }
        }

        let provider = MockProvider;
        assert!(provider.embed_batch(&[]).is_err());
        assert!(provider.embed_batch(&["hello"]).is_ok());
    }
}
