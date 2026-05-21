// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::Path, sync::Arc};

use axum::extract::{Json, State};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, FileSystem, RuntimeTypeGenerator, StdFileSystem,
    StdPackager, TscCompiler,
};
use baml_rt_core::join_error_message;
use baml_rt_repository::{
    RepositoryService,
    commands::{PublishCommand, PublishResult},
    entry::SourceBundle,
    package::source_bundle_from_tar_gz,
};
// Register tool inventory for repository-side artifact builds (POST /repository/publish).
use baml_tools_notion as _;
use baml_tools_security_eval as _;
use http_api_problem::HttpApiProblem;

/// Publish an agent: build from source and store in repository (POST /repository/publish, operator-authenticated).
pub async fn publish_with_build(
    State(svc): State<Arc<RepositoryService>>,
    Json(cmd): Json<PublishCommand>,
) -> Result<Json<PublishResult>, HttpApiProblem> {
    let next_version = svc
        .next_version_for_agent(&cmd.name)
        .await
        .map_err(HttpApiProblem::from)?;

    let source_versioned = cmd.source.with_manifest_version(next_version);
    tracing::info!(
        agent = %cmd.name,
        repository_version = next_version,
        "building repository artifact"
    );
    let built: Arc<[u8]> = Arc::from(
        build_artifact(source_versioned, svc.clone())
            .await
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Artifact build failed")
                    .detail(e.to_string())
            })?,
    );

    let parse_bytes = Arc::clone(&built);
    let (_, extracted) =
        tokio::task::spawn_blocking(move || source_bundle_from_tar_gz(parse_bytes.as_ref()))
            .await
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Artifact parse failed")
                    .detail(join_error_message("artifact parse", e))
            })?
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Built artifact did not parse as a source bundle")
                    .detail(e.to_string())
            })?;

    let expected = extracted.with_manifest_version(next_version).compute_hash();

    let cmd2 = PublishCommand {
        name: cmd.name,
        source: extracted,
        rationale: cmd.rationale,
        origin: cmd.origin,
    };
    let result = svc.publish(cmd2).await.map_err(HttpApiProblem::from)?;
    if expected.as_str() != result.hash.as_str() {
        return Err(
            HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR).detail(
                format!(
                    "internal invariant: published hash {} != canonical hash {} from packaged artifact (same rules as insert_entry)",
                    result.hash.as_str(),
                    expected.as_str()
                ),
            ),
        );
    }

    svc.put_built_blob(&result.hash, built.as_ref())
        .await
        .map_err(HttpApiProblem::from)?;

    Ok(Json(result))
}

async fn build_artifact(
    source: SourceBundle,
    svc: Arc<RepositoryService>,
) -> anyhow::Result<Vec<u8>> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(build_artifact_inner(&source, svc)))
        .await
        .map_err(|e| anyhow::anyhow!("{}", join_error_message("artifact build", e)))?
}

async fn build_artifact_inner(
    source: &SourceBundle,
    svc: Arc<RepositoryService>,
) -> anyhow::Result<Vec<u8>> {
    let workspace = unique_temp_dir("baml-repository-publish");
    fs::create_dir_all(&workspace)?;

    let result = async {
        materialize_source_bundle(source, &workspace)?;
        let manifest_tools = source.manifest.tools();
        let mcp_tools: Vec<&str> = manifest_tools
            .iter()
            .copied()
            .filter(|tool| tool.starts_with("mcp/"))
            .collect();
        if !mcp_tools.is_empty() {
            tracing::info!(
                mcp_tools = ?mcp_tools,
                "repository build will resolve MCP tools from registry"
            );
        }

        let agent_dir = AgentDir::new(workspace.clone())?;
        let build_dir = BuildDir::new()?;
        let fs_impl = StdFileSystem;
        fs_impl.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;
        let output = build_dir.join("package.tar.gz");
        let builder = BuilderService::new(
            TscCompiler::new(),
            RuntimeTypeGenerator::with_mcp_registry_service(svc),
            StdPackager::new(),
        );
        builder
            .build_package(&agent_dir, &build_dir, &output)
            .await
            .map_err(anyhow::Error::from)?;

        let bytes = fs::read(&output)?;
        Ok::<_, anyhow::Error>(bytes)
    }
    .await;

    let _ = fs::remove_dir_all(&workspace);
    result
}

fn materialize_source_bundle(source: &SourceBundle, root: &Path) -> anyhow::Result<()> {
    let manifest_path = root.join("manifest.json");
    let manifest = serde_json::to_string_pretty(source.manifest.as_value())?;
    fs::write(&manifest_path, manifest)?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("baml_src"))?;

    for file in source.ts_sources.iter().chain(source.baml_sources.iter()) {
        let path = root.join(file.path.as_str());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, file.content.as_str())?;
    }
    Ok(())
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}
