// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Export repository tool catalogs into the unified offline snapshot-cache layout.

use std::path::Path;

use anyhow::{Context, Result, bail};
use baml_rt_tools::{
    StaticToolCatalogResponse, external_tools::ExternalToolSnapshot,
    mcp_snapshot::McpServerSnapshot,
};
use console::style;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct ExternalSnapshotsResponse {
    snapshots: Vec<ExternalToolSnapshot>,
}

pub fn run(repository_url: &str, output: &str) -> Result<()> {
    let output = Path::new(output);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(export_all(repository_url, output))
}

async fn export_all(repository_url: &str, output: &Path) -> Result<()> {
    let http = reqwest::Client::new();

    let static_catalog: StaticToolCatalogResponse =
        get_json(&http, repository_url, "/static-tools/snapshots").await?;
    let static_count = static_catalog.tools.len();
    baml_rt_builder::static_tool_registry::write_static_tool_catalog_to_cache(
        output,
        &static_catalog,
    )
    .context("writing static tool catalog to snapshot cache")?;

    let external: ExternalSnapshotsResponse =
        get_json(&http, repository_url, "/external-tools/snapshots").await?;
    let mut external_count = 0usize;
    for snapshot in external.snapshots {
        if snapshot.approval.state.is_approved() {
            baml_rt_tools::external_tool_cache::write_approved_snapshot(output, &snapshot)
                .with_context(|| {
                    format!(
                        "writing external-tool snapshot {} to snapshot cache",
                        snapshot.tool.name
                    )
                })?;
            external_count += 1;
        }
    }

    let servers_body: Value = get_json(&http, repository_url, "/mcp/servers").await?;
    let servers = servers_body
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("/mcp/servers response missing servers[]"))?;
    let mut mcp_count = 0usize;
    let mcp_root = output.join("mcp");
    for server in servers {
        let Some(server_id) = server.get("server_id").and_then(Value::as_str) else {
            continue;
        };
        let path = format!("/mcp/servers/{server_id}");
        let snapshot: McpServerSnapshot = get_json(&http, repository_url, &path)
            .await
            .with_context(|| {
                format!("fetching MCP snapshot {server_id} from {path} for snapshot cache export")
            })?;
        if !snapshot.approval.state.is_approved() {
            continue;
        }
        baml_rt_tools::mcp_cache::write_snapshot(&mcp_root, &snapshot)
            .with_context(|| format!("writing MCP snapshot {server_id} to snapshot cache"))?;
        mcp_count += 1;
    }

    println!(
        "{} Exported snapshot cache to {}",
        style("Done!").green().bold(),
        style(output.display()).cyan()
    );
    println!("  static tools: {static_count}");
    println!("  external tool snapshots: {external_count}");
    println!("  MCP server snapshots: {mcp_count}");
    Ok(())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    repository_url: &str,
    path: &str,
) -> Result<T> {
    let url = format!("{}{}", repository_url.trim_end_matches('/'), path);
    let response = http
        .get(url.as_str())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GET {url} failed ({status}): {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    use super::*;

    fn start_snapshot_server_with_failing_mcp_snapshot() -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let app = Router::new()
                    .route(
                        "/static-tools/snapshots",
                        get(|| async {
                            Json(json!({
                                "schema_version": "static-tool-catalog.v1",
                                "tools": []
                            }))
                        }),
                    )
                    .route(
                        "/external-tools/snapshots",
                        get(|| async { Json(json!({ "snapshots": [] })) }),
                    )
                    .route(
                        "/mcp/servers",
                        get(|| async { Json(json!({ "servers": [{ "server_id": "meteo" }] })) }),
                    )
                    .route(
                        "/mcp/servers/meteo",
                        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
                    );
                let (listener, addr) = test_support::common::bind_ephemeral_tokio("127.0.0.1")
                    .await
                    .unwrap();
                tx.send(addr.port()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });

        let port = rx.recv().unwrap();
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn export_all_fails_when_mcp_snapshot_fetch_fails() {
        let repository_url = start_snapshot_server_with_failing_mcp_snapshot();
        let output = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let err = rt
            .block_on(export_all(&repository_url, output.path()))
            .unwrap_err();
        let chained = format!("{err:#}");

        assert!(
            chained.contains(
                "fetching MCP snapshot meteo from /mcp/servers/meteo for snapshot cache export"
            ),
            "missing MCP context: {chained}"
        );
        assert!(
            chained.contains("GET") && chained.contains("/mcp/servers/meteo failed"),
            "missing HTTP failure: {chained}"
        );
    }
}
