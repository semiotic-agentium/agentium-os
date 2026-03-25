//! Utilities for working with packaged agent artifacts (`.tar.gz`).

use std::io::{Cursor, Read};

use flate2::read::GzDecoder;
use serde_json::Value;
use tar::Archive;
use thiserror::Error;

use crate::{
    entry::{ManifestSource, SourceBundle, SourceContent, SourceFile, SourcePath},
    ids::AgentName,
};

/// Errors produced while extracting repository source data from a package blob.
#[derive(Debug, Error)]
pub enum PackageExtractError {
    #[error("failed to read tar entries: {0}")]
    TarEntries(String),
    #[error("failed to read tar entry: {0}")]
    TarEntry(String),
    #[error("invalid tar entry path: {0}")]
    TarEntryPath(String),
    #[error("manifest.json missing in package")]
    ManifestMissing,
    #[error("failed to read manifest.json as UTF-8: {path}")]
    ManifestRead { path: String },
    #[error("failed to parse manifest.json: {0}")]
    ManifestParse(String),
    #[error("manifest.json missing required string field: name")]
    ManifestNameMissing,
    #[error("manifest.json contains invalid agent name: {0}")]
    ManifestNameInvalid(String),
    #[error("failed to read source file as UTF-8: {path}")]
    SourceRead { path: String },
    #[error("invalid source path '{path}': {reason}")]
    SourcePathInvalid { path: String, reason: String },
}

/// Extract `(AgentName, SourceBundle)` from a packaged `.tar.gz` archive.
///
/// Reads:
/// - `manifest.json`
/// - `*.ts`
/// - `baml_src/*.baml`
pub fn source_bundle_from_tar_gz(
    bytes: &[u8],
) -> std::result::Result<(AgentName, SourceBundle), PackageExtractError> {
    let cursor = Cursor::new(bytes);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);

    let mut manifest: Option<Value> = None;
    let mut ts_sources: Vec<SourceFile> = Vec::new();
    let mut baml_sources: Vec<SourceFile> = Vec::new();

    for entry_result in archive
        .entries()
        .map_err(|e| PackageExtractError::TarEntries(e.to_string()))?
    {
        let mut entry = entry_result.map_err(|e| PackageExtractError::TarEntry(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| PackageExtractError::TarEntryPath(e.to_string()))?
            .to_string_lossy()
            .to_string();

        if path.ends_with('/') {
            continue;
        }

        if path == "manifest.json" {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .map_err(|_| PackageExtractError::ManifestRead { path: path.clone() })?;
            manifest = Some(
                serde_json::from_str(&content)
                    .map_err(|e| PackageExtractError::ManifestParse(e.to_string()))?,
            );
            continue;
        }

        let is_ts = path.ends_with(".ts");
        let is_baml = path.starts_with("baml_src/") && path.ends_with(".baml");
        if !is_ts && !is_baml {
            continue;
        }

        let mut content = String::new();
        entry
            .read_to_string(&mut content)
            .map_err(|_| PackageExtractError::SourceRead { path: path.clone() })?;
        let source_file = SourceFile {
            path: SourcePath::new(path.clone()).map_err(|e| {
                PackageExtractError::SourcePathInvalid {
                    path: path.clone(),
                    reason: e.reason.to_string(),
                }
            })?,
            content: SourceContent::new(content),
        };
        if is_ts {
            ts_sources.push(source_file);
        } else {
            baml_sources.push(source_file);
        }
    }

    let manifest = manifest.ok_or(PackageExtractError::ManifestMissing)?;
    let name = manifest
        .get("name")
        .and_then(Value::as_str)
        .ok_or(PackageExtractError::ManifestNameMissing)?
        .parse::<AgentName>()
        .map_err(|e| PackageExtractError::ManifestNameInvalid(e.to_string()))?;

    Ok((
        name,
        SourceBundle {
            manifest: ManifestSource::new(manifest),
            ts_sources,
            baml_sources,
        },
    ))
}
