//! Pre-download fastembed ONNX model files to a local cache directory.
//!
//! Run once to populate `models/fastembed/` in the repo root so all subsequent
//! runs load without network access:
//!
//! ```sh
//! cargo run -p baml-rt-embedding --bin download_models
//! ```
//!
//! The target directory defaults to `<workspace-root>/models/fastembed` and
//! can be overridden with `BAML_MODELS_DIR` (the binary appends `/fastembed`).
//!
//! Models downloaded:
//! - `Alibaba-NLP/gte-base-en-v1.5` (768-d bi-encoder for drift scoring, ~500 MB)
//! - `jinaai/jina-reranker-v1-turbo-en` (cross-encoder for plan drift, ~100 MB)

use std::path::PathBuf;

use fastembed::{
    EmbeddingModel, InitOptions, RerankInitOptions, RerankerModel, TextEmbedding, TextRerank,
};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/baml-rt-embedding`; go up two levels to workspace root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn target_dir() -> PathBuf {
    let base = std::env::var("BAML_MODELS_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("models"));
    base.join("fastembed")
}

fn main() {
    let dir = target_dir();
    std::fs::create_dir_all(&dir).expect("create models/fastembed dir");
    println!("Downloading models to: {}", dir.display());

    println!("\n[1/2] gte-base-en-v1.5  (bi-encoder, ~500 MB)…");
    let opts = InitOptions::new(EmbeddingModel::GTEBaseENV15)
        .with_cache_dir(dir.clone())
        .with_show_download_progress(true);
    TextEmbedding::try_new(opts).expect("gte-base-en-v1.5 download");
    println!("      ✓ done");

    println!("\n[2/2] jina-reranker-v1-turbo-en  (cross-encoder, ~100 MB)…");
    let opts = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
        .with_cache_dir(dir.clone())
        .with_show_download_progress(true);
    TextRerank::try_new(opts).expect("jina-reranker download");
    println!("      ✓ done");

    println!("\nAll models cached at {}", dir.display());
    println!(
        "Set BAML_MODELS_DIR={} in .env to skip future downloads.",
        dir.parent().unwrap_or(&dir).display()
    );
}
