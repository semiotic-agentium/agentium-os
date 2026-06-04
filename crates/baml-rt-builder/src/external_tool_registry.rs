// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Reusable external-tool registry operator helpers.
//!
//! Mirrors [`crate::mcp_registry`]: post an approved snapshot into the registry
//! and fetch approved snapshots back out as a builder catalog source.

use anyhow::{Context, Result, bail};
use baml_rt_repository::http::ImportExternalToolSnapshotRequest;
use baml_rt_tools::external_tools::{ExternalToolRegistryCatalog, ExternalToolSnapshot};

pub use crate::mcp_registry::{resolve_runner_token, resolve_runner_token_from_sources};

fn check_response(
    status: reqwest::StatusCode,
    body: &str,
    op_name: &str,
    authenticated: bool,
) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let hint = if authenticated {
            "Hint: the runner token was rejected \
             — verify it matches the server's RUNNER_TOKEN."
        } else {
            "Hint: pass --runner-token <token> or set the \
             RUNNER_TOKEN environment variable."
        };
        bail!("{op_name} failed ({status}): {body}. {hint}");
    }
    if !status.is_success() {
        bail!("{op_name} failed ({status}): {body}");
    }
    Ok(())
}

/// POST an approved external-tool snapshot to `/external-tools/snapshots/import`.
///
/// Returns the parsed JSON response (`{ "version": ExternalToolRegistryToolVersion }`).
pub async fn post_external_tool_snapshot_to_registry(
    repository_url: &str,
    snapshot: ExternalToolSnapshot,
    runner_token_flag: Option<&str>,
    op_name: &str,
) -> Result<serde_json::Value> {
    let token = resolve_runner_token(runner_token_flag)?;
    let url = format!(
        "{}/external-tools/snapshots/import",
        repository_url.trim_end_matches('/')
    );
    let http = reqwest::Client::new();
    let mut request = http
        .post(url.as_str())
        .json(&ImportExternalToolSnapshotRequest { snapshot });
    if let Some(ref token) = token {
        request = request.header("X-Runner-Token", token.as_str());
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("posting external tool snapshot to {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    check_response(status, &body, op_name, token.is_some())?;
    serde_json::from_str(&body).context("Failed to parse external tool registry response")
}

/// Fetch all latest-approved external-tool snapshots from the registry and
/// project them into a builder [`ExternalToolRegistryCatalog`].
pub async fn fetch_external_tool_registry_catalog(
    repository_url: &str,
) -> Result<ExternalToolRegistryCatalog> {
    let url = format!(
        "{}/external-tools/snapshots",
        repository_url.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(url.as_str())
        .send()
        .await
        .with_context(|| format!("fetching external tool snapshots from {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("failed to fetch external tool snapshots from {url} ({status}): {body}");
    }

    let mut value: serde_json::Value =
        serde_json::from_str(&body).context("parsing external tool snapshots response")?;
    let snapshots_value = value
        .get_mut("snapshots")
        .map(serde_json::Value::take)
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let snapshots: Vec<ExternalToolSnapshot> = serde_json::from_value(snapshots_value)
        .context("decoding external tool snapshots from registry response")?;
    ExternalToolRegistryCatalog::from_snapshots(snapshots)
        .map_err(|e| anyhow::anyhow!("projecting external tool registry snapshots: {e}"))
}
