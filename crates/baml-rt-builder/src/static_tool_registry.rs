// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Static-tool catalog fetcher and offline loader.
//!
//! Mirrors [`crate::external_tool_registry`]: fetch the runner's compiled-in
//! static-tool catalog over HTTP, or load a previously-exported catalog JSON
//! from disk for offline builds. Both project the wire
//! [`StaticToolCatalogResponse`] into a builder [`StaticToolSnapshotCatalog`]
//! that typegen consumes in place of the CLI's own link-time inventory.

use std::path::Path;

use anyhow::{Context, Result, bail};
use baml_rt_tools::{StaticToolCatalogResponse, StaticToolSnapshotCatalog};

/// Fetch the runner's static-tool catalog from
/// `{repository_url}/static-tools/snapshots` and project it into a builder
/// catalog.
///
/// The endpoint returns a bare [`StaticToolCatalogResponse`] (not wrapped). A
/// 404 means the target repository has no host-runner inventory (e.g. a
/// detached repository) — surfaced as an error rather than a silent empty
/// catalog, which would recreate the stale-schema bug this whole path exists to
/// kill.
pub async fn fetch_static_tool_catalog(repository_url: &str) -> Result<StaticToolSnapshotCatalog> {
    let url = format!(
        "{}/static-tools/snapshots",
        repository_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(url.as_str())
        .send()
        .await
        .with_context(|| format!("fetching static tool catalog from {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "static tool catalog not available at {url} ({status}): {body}. \
             The target repository has no host-runner inventory; point \
             --repository-url at a running runner."
        );
    }
    if !status.is_success() {
        bail!("failed to fetch static tool catalog from {url} ({status}): {body}");
    }

    let response: StaticToolCatalogResponse =
        serde_json::from_str(&body).context("decoding static tool catalog response")?;
    StaticToolSnapshotCatalog::from_response(response)
        .map_err(|e| anyhow::anyhow!("projecting static tool catalog: {e}"))
}

/// Load a previously-exported static-tool catalog JSON from disk and project it
/// into a builder catalog. Used for offline builds (`--static-tool-catalog`).
pub fn load_static_tool_catalog_from_file(
    path: impl AsRef<Path>,
) -> Result<StaticToolSnapshotCatalog> {
    let path = path.as_ref();
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading static tool catalog from {}", path.display()))?;
    let response: StaticToolCatalogResponse = serde_json::from_str(&body)
        .with_context(|| format!("decoding static tool catalog from {}", path.display()))?;
    StaticToolSnapshotCatalog::from_response(response)
        .map_err(|e| anyhow::anyhow!("projecting static tool catalog: {e}"))
}
