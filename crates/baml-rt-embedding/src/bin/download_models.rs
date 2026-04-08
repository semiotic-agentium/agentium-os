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

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
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

fn looks_like_lfs_pointer(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let probe = &bytes[..bytes.len().min(256)];
    let Ok(text) = std::str::from_utf8(probe) else {
        return false;
    };
    text.starts_with("version https://git-lfs.github.com/spec/v1")
}

fn dir_contains_lfs_pointer_onnx(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains_lfs_pointer_onnx(&path) {
                return true;
            }
            continue;
        }

        if path.extension().and_then(|e| e.to_str()) == Some("onnx")
            && looks_like_lfs_pointer(&path)
        {
            return true;
        }
    }

    false
}

fn purge_if_lfs_pointer(cache_dir: &Path, model_dir_name: &str) -> Result<()> {
    let model_dir = cache_dir.join(model_dir_name);
    if !model_dir.exists() {
        return Ok(());
    }

    if dir_contains_lfs_pointer_onnx(&model_dir) {
        println!(
            "Detected Git LFS pointer file(s) under {} — removing stale cache so fastembed can download real ONNX binaries...",
            model_dir.display()
        );
        fs::remove_dir_all(&model_dir).with_context(|| {
            format!(
                "remove stale model cache directory with LFS pointer files: {}",
                model_dir.display()
            )
        })?;
    }

    Ok(())
}

fn download_embedding(cache_dir: &Path) -> Result<()> {
    purge_if_lfs_pointer(cache_dir, "models--Alibaba-NLP--gte-base-en-v1.5")?;

    let opts = InitOptions::new(EmbeddingModel::GTEBaseENV15)
        .with_cache_dir(cache_dir.to_path_buf())
        .with_show_download_progress(true);

    match TextEmbedding::try_new(opts) {
        Ok(_) => Ok(()),
        Err(first_err) => {
            // If the first attempt fails (often due to stale/corrupt cache), clear and retry once.
            let model_dir = cache_dir.join("models--Alibaba-NLP--gte-base-en-v1.5");
            if model_dir.exists() {
                let _ = fs::remove_dir_all(&model_dir);
            }
            let opts = InitOptions::new(EmbeddingModel::GTEBaseENV15)
                .with_cache_dir(cache_dir.to_path_buf())
                .with_show_download_progress(true);
            TextEmbedding::try_new(opts).with_context(|| {
                format!(
                    "gte-base-en-v1.5 download failed after cache reset (first error: {first_err})"
                )
            })?;
            Ok(())
        }
    }
}

fn download_reranker(cache_dir: &Path) -> Result<()> {
    purge_if_lfs_pointer(cache_dir, "models--jinaai--jina-reranker-v1-turbo-en")?;

    let opts = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
        .with_cache_dir(cache_dir.to_path_buf())
        .with_show_download_progress(true);

    match TextRerank::try_new(opts) {
        Ok(_) => Ok(()),
        Err(first_err) => {
            let model_dir = cache_dir.join("models--jinaai--jina-reranker-v1-turbo-en");
            if model_dir.exists() {
                let _ = fs::remove_dir_all(&model_dir);
            }
            let opts = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
                .with_cache_dir(cache_dir.to_path_buf())
                .with_show_download_progress(true);
            TextRerank::try_new(opts).with_context(|| {
                format!(
                    "jina-reranker-v1-turbo-en download failed after cache reset (first error: {first_err})"
                )
            })?;
            Ok(())
        }
    }
}

fn main() -> Result<()> {
    let dir = target_dir();
    fs::create_dir_all(&dir).context("create models/fastembed dir")?;
    println!("Downloading models to: {}", dir.display());

    println!("\n[1/2] gte-base-en-v1.5  (bi-encoder, ~500 MB)…");
    download_embedding(&dir)?;
    println!("      ✓ done");

    println!("\n[2/2] jina-reranker-v1-turbo-en  (cross-encoder, ~100 MB)…");
    download_reranker(&dir)?;
    println!("      ✓ done");

    println!("\nAll models cached at {}", dir.display());
    println!(
        "Set BAML_MODELS_DIR={} in .env to skip future downloads.",
        dir.parent().unwrap_or(&dir).display()
    );

    Ok(())
}
